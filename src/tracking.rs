use std::collections::HashMap;

use redis::aio::Connection;
use redis::{AsyncCommands, Client};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::center::CenterId;

pub type UserId = u64;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserData {
  pub subscriptions: Vec<CenterId>,
  pub chat_id: i64,
}

impl From<(Vec<u32>, i64)> for UserData {
  fn from((subscriptions, chat_id): (Vec<u32>, i64)) -> Self {
    Self { subscriptions, chat_id }
  }
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
struct AllUsers {
  pub list: Vec<UserId>,
}

impl From<Vec<UserId>> for AllUsers {
  fn from(list: Vec<UserId>) -> Self {
    Self { list }
  }
}

pub struct TrackingManager {
  db_connection: Connection,
  user_data: HashMap<UserId, UserData>,
  all_users: AllUsers,
}

impl TrackingManager {
  pub async fn new(client: Client) -> Self {
    let mut s = Self {
      db_connection: client.get_async_connection().await.unwrap(),
      user_data: HashMap::new(),
      all_users: AllUsers::default(),
    };

    s.sync_all_users().await;

    for user in s.all_users.list.clone() {
      if let Some(user_data) = s.get_db_user_data(user).await {
        s.user_data.insert(user, user_data);
      } else {
        warn!(
          "Attempted to populate user data but could not get user data for {}",
          user
        );
      }
    }

    s
  }

  async fn get_db_user_data(&mut self, user: UserId) -> Option<UserData> {
    let user_data: Result<String, _> = self.db_connection.get(user).await;

    if let Ok(user_data) = user_data {
      info!("{}", user_data);
      let data: Result<UserData, _> = toml::from_str(user_data.as_str());
      if let Ok(user_data) = data {
        Some(user_data)
      } else {
        warn!("Could not parse user data");
        None
      }
    } else {
      warn!("{}", user_data.unwrap_err().to_string());
      None
    }
  }

  async fn ensure_user_in_list(&mut self, user: UserId) {
    info!("Ensuring {} is in all users list", user);
    let all_users: Result<String, _> = self.db_connection.get("all_users").await;

    info!("{:?}", all_users);
    if let Ok(all_users) = all_users {
      let data: Result<AllUsers, _> = toml::from_str(all_users.as_str());
      if let Ok(mut all_users) = data {
        if !all_users.list.contains(&user) {
          all_users.list.push(user);
          self.all_users = all_users.clone();
          let all_users: String = toml::to_string(&all_users).unwrap();
          let _: Result<(), _> = self.db_connection.set("all_users", all_users).await;
        }
        return;
      } else {
        warn!("Failed to parse all users");
      }
    }

    warn!("Failed to get all users, defaulting to new list. Hopefully this is expected");
    let _: Result<(), _> = self
      .db_connection
      .set("all_users", toml::to_string(&AllUsers::from(vec![user])).unwrap())
      .await;
  }

  async fn set_db_user_data(&mut self, user: UserId, user_data: UserData) -> Result<(), String> {
    let user_data: String = toml::to_string(&user_data).unwrap();
    self.ensure_user_in_list(user).await;
    self.db_connection.set(user, user_data).await.map_err(|x| x.to_string())
  }

  async fn sync_all_users(&mut self) {
    info!("Syncing all users...");
    let all_users: Result<String, _> = self.db_connection.get("all_users").await;
    if let Ok(all_users) = all_users {
      if let Ok(all_users) = toml::from_str(all_users.as_str()) {
        self.all_users = all_users;
      } else {
        warn!("Could not parse all users list from db!");
      }
    } else {
      warn!("Could not get all users!");
    }
  }

  async fn sync_with_db(&mut self, user: UserId) -> Result<(), String> {
    info!("Getting data for user id {}", user);

    self.sync_all_users().await;

    if let Some(user_data) = self.get_db_user_data(user).await {
      self.user_data.insert(user, user_data);
    } else {
      warn!("Could not find or parse user data");
    }

    Ok(())
  }

  pub async fn track_center(&mut self, channel_id: i64, user: UserId, center: CenterId) -> Result<(), String> {
    if let Err(err) = self.sync_with_db(user).await {
      return Err(err);
    }

    let current_list = self.user_data.get_mut(&user).cloned();
    if let Some(mut current_list) = current_list {
      if current_list.subscriptions.contains(&center) {
        Err("You are already tracking this center.".to_string())
      } else {
        current_list.subscriptions.push(center);
        self.user_data.insert(user, current_list.clone());
        self.set_db_user_data(user, current_list).await
      }
    } else {
      let list = Vec::from([center]);
      let user_data = UserData::from((list, channel_id));
      self.user_data.insert(user, user_data.clone());
      self.set_db_user_data(user, user_data).await
    }
  }

  pub async fn untrack_center(&mut self, user: UserId, center: CenterId) -> Result<(), String> {
    if let Err(err) = self.sync_with_db(user).await {
      return Err(err);
    }

    let current_list = self.user_data.get_mut(&user).cloned();
    if let Some(mut current_list) = current_list {
      if let Some(index) = current_list.subscriptions.iter().position(|&x| x == center) {
        current_list.subscriptions.remove(index);
        self.user_data.insert(user, current_list.clone());
        self.set_db_user_data(user, current_list).await
      } else {
        Err("You are not tracking this center!".to_string())
      }
    } else {
      Err("You are not tracking any centers!".to_string())
    }
  }

  pub async fn get_user_data(&mut self, user: UserId) -> Result<Option<&UserData>, String> {
    if let Err(err) = self.sync_with_db(user).await {
      return Err(err);
    }

    Ok(self.user_data.get(&user))
  }

  pub fn get_center_subscribers(&mut self) -> HashMap<CenterId, Vec<UserId>> {
    let mut result: HashMap<u32, Vec<u64>> = HashMap::new();

    for user in self.all_users.list.iter() {
      if let Some(user_data) = self.user_data.get(user) {
        for center in user_data.subscriptions.iter() {
          if let Some(list) = result.get_mut(center) {
            list.push(*user);
          } else {
            result.insert(*center, vec![*user]);
          }
        }
      }
    }

    result
  }
}

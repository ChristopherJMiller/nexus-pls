use std::collections::HashMap;
use std::env;
use std::error::Error;

use center::CentersConfig;
use lazy_static::lazy_static;
use redis::Client;
use teloxide::prelude::*;
use teloxide::types::{MessageKind, ParseMode};
use teloxide::utils::command::BotCommands;
use tokio::sync::Mutex;
use tracing::info;

use crate::center::{Center, CenterDataCollectorTask, CenterId};
use crate::tracking::TrackingManager;
mod center;
mod tracking;

lazy_static! {
  static ref CENTERS: Vec<Center> = toml::from_str::<CentersConfig>(include_str!("../centers.toml"))
    .unwrap()
    .centers;
  static ref CENTER_LUT: HashMap<CenterId, Center> =
    CENTERS.clone().into_iter().map(|x: Center| (x.id, x)).collect::<_>();
  pub static ref MANAGER: Mutex<Option<TrackingManager>> = Mutex::new(None);
}

#[tokio::main]
async fn main() {
  tracing_subscriber::fmt::init();
  info!("Starting Nexus Pls");

  {
    info!("Configuring Tracking Manager");
    let mut lock = MANAGER.lock().await;
    *lock = Some(TrackingManager::new(Client::open("redis://127.0.0.1/").unwrap()).await);
    info!("Finished Configuring Tracking Manager");
  }

  info!("Configuring Https Client");
  let https = hyper_rustls::HttpsConnectorBuilder::new()
    .with_native_roots()
    .https_only()
    .enable_http1()
    .build();
  let client = hyper::Client::builder().build::<_, hyper::Body>(https);

  info!("Configuring Telegram Bot");
  if env::var("TELOXIDE_TOKEN").is_err() {
    panic!("Could not parse or find Bot token TELOXIDE_TOKEN");
  }
  let bot = Bot::from_env().auto_send();
  info!("Telegram Bot Configured");

  info!("Starting Async Jobs");
  tokio::select! {
    _ = CenterDataCollectorTask::new(client, bot.clone()) => {},
    _ = teloxide::commands_repl(bot, answer, Command::ty()) => {}
  };
  info!("Exiting, Goodbye!");
}

#[derive(BotCommands, Clone)]
#[command(rename = "lowercase", description = "These commands are supported:")]
enum Command {
  #[command(description = "display this text.")]
  Help,
  #[command(description = "list centers to track.")]
  List,
  #[command(description = "begins to track a center on your behalf.")]
  Track(String),
  #[command(description = "stops tracking a center on your behalf.")]
  UnTrack(String),
  #[command(description = "lists the status of your tracked centers.")]
  Status,
}

async fn answer(bot: AutoSend<Bot>, message: Message, command: Command) -> Result<(), Box<dyn Error + Send + Sync>> {
  match command {
    Command::Help => {
      bot
        .send_message(message.chat.id, Command::descriptions().to_string())
        .await?
    },
    Command::List => {
      let mut center_list = CENTERS.iter().map(|x| format!("{}", x)).collect::<Vec<_>>();
      center_list.sort_by(|a, b| a.cmp(b));
      bot
        .send_message(message.chat.id, center_list.join("\n"))
        .parse_mode(ParseMode::MarkdownV2)
        .await?
    },
    Command::Track(center) => {
      let mut user: Option<u64> = None;

      if let MessageKind::Common(message) = message.kind {
        if let Some(from_user) = message.from {
          user = Some(from_user.id.0);
        }
      }

      let center = CENTERS.iter().find(|&x| x.short_name == center);

      if center.is_none() {
        bot
          .send_message(message.chat.id, "Could not find center".to_string())
          .await?
      } else if user.is_none() {
        bot
          .send_message(message.chat.id, "Could not understand who sent this?".to_string())
          .await?
      } else {
        let user = user.unwrap();

        if let Err(err) = MANAGER
          .lock()
          .await
          .as_mut()
          .unwrap()
          .track_center(message.chat.id.0, user, center.unwrap().id)
          .await
        {
          bot.send_message(message.chat.id, err).await?
        } else {
          bot
            .send_message(
              message.chat.id,
              format!("Now tracking {} on your behalf", center.unwrap().full_name),
            )
            .await?
        }
      }
    },
    Command::UnTrack(center) => {
      let mut user: Option<u64> = None;

      if let MessageKind::Common(message) = message.kind {
        if let Some(from_user) = message.from {
          user = Some(from_user.id.0);
        }
      }

      let center = CENTERS.iter().find(|&x| x.short_name == center);

      if center.is_none() {
        bot
          .send_message(message.chat.id, "Could not find center".to_string())
          .await?
      } else if user.is_none() {
        bot
          .send_message(message.chat.id, "Could not understand who sent this?".to_string())
          .await?
      } else {
        let user = user.unwrap();

        if let Err(err) = MANAGER
          .lock()
          .await
          .as_mut()
          .unwrap()
          .untrack_center(user, center.unwrap().id)
          .await
        {
          bot.send_message(message.chat.id, err).await?
        } else {
          bot
            .send_message(
              message.chat.id,
              format!("Stopped tracking {} on your behalf", center.unwrap().full_name),
            )
            .await?
        }
      }
    },
    Command::Status => {
      let mut user: Option<u64> = None;

      if let MessageKind::Common(message) = message.kind {
        if let Some(from_user) = message.from {
          user = Some(from_user.id.0);
        }
      }

      if user.is_none() {
        bot
          .send_message(message.chat.id, "Could not understand who sent this?".to_string())
          .await?
      } else {
        if let Ok(list) = MANAGER
          .lock()
          .await
          .as_mut()
          .unwrap()
          .get_user_data(user.unwrap())
          .await
        {
          let mut center_list = list
            .map_or(&Vec::new(), |u| &u.subscriptions)
            .iter()
            .map(|x| CENTER_LUT.get(x))
            .filter(|x| x.is_some())
            .map(|x| format!("{}", x.unwrap()))
            .collect::<Vec<_>>();
          center_list.sort_by(|a, b| a.cmp(b));

          if center_list.len() == 0 {
            center_list.push("None".to_string());
          }

          bot
            .send_message(
              message.chat.id,
              format!("Your Tracked Centers\n{}", center_list.join("\n")),
            )
            .parse_mode(ParseMode::MarkdownV2)
            .await?
        } else {
          bot
            .send_message(message.chat.id, "Failed to get user tracking subscriptions".to_string())
            .await?
        }
      }
    },
  };

  Ok(())
}

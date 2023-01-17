use std::fmt::Display;
use std::future::Future;
use std::pin::Pin;
use std::sync::mpsc::{self, Receiver, Sender};
use std::task::{Context, Poll};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{NaiveDateTime, Datelike, NaiveDate};
use hyper::client::HttpConnector;
use hyper::{Client, Uri};
use hyper_rustls::HttpsConnector;
use serde::Deserialize;
use teloxide::adaptors::AutoSend;
use teloxide::payloads::SendMessageSetters;
use teloxide::prelude::Requester;
use teloxide::types::{ChatId, ParseMode, Recipient};
use teloxide::utils::markdown::escape;
use teloxide::Bot;
use tracing::{info, warn};

use crate::{CENTER_LUT, MANAGER};

pub type CenterId = u32;

#[derive(Deserialize, Clone)]
pub struct Center {
  pub id: CenterId,
  pub short_name: String,
  pub full_name: String,
  pub address: String,
}

impl Display for Center {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "`{}` {}", escape(&self.short_name), escape(&self.full_name))
  }
}

impl Center {
  fn appointment_avaliable_msg(&self, slot: &Slot) -> String {
    let timeslot = NaiveDateTime::parse_from_str(&slot.start_timestamp, "%Y-%m-%dT%H:%M").unwrap();
    let timeslot = timeslot.format("%l:%M %p on %A %B %-d").to_string();
    let link = "https://ttp.cbp.dhs.gov/schedulerui/schedule-interview/location?lang=en&vo=true&returnUrl=ttp-external&service=nh";
    format!(
      "Appointment Avaliable for {}\n{}\n[Schedule Appointment]({})",
      self.full_name, timeslot, link
    )
  }
}

#[derive(Deserialize)]
pub struct CentersConfig {
  pub centers: Vec<Center>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Slot {
  pub location_id: u32,
  pub start_timestamp: String,
}

type ScheduleSlots = Vec<Slot>;

#[derive(Debug, Clone)]
enum CollectorMessage {
  RequestSlotsForCenter(CenterId),
  NotifyUsersOf(CenterId, Vec<Slot>),
  Stop,
}

pub struct CenterDataCollectorTask {
  next_collection_time: Option<Instant>,
  tx: Sender<CollectorMessage>,
}

impl CenterDataCollectorTask {
  pub fn new(http_client: Client<HttpsConnector<HttpConnector>>, bot: AutoSend<Bot>) -> Self {
    let (tx, rx) = mpsc::channel();
    CenterDataCollectorTask::spawn_worker_thread(http_client, bot, tx.clone(), rx);
    Self {
      next_collection_time: None,
      tx,
    }
  }

  fn spawn_worker_thread(
    http_client: Client<HttpsConnector<HttpConnector>>,
    bot: AutoSend<Bot>,
    tx: Sender<CollectorMessage>,
    rx: Receiver<CollectorMessage>,
  ) {
    thread::spawn(move || {
      tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
          info!("Async Worker Thread Started");
          loop {
            while let Ok(msg) = rx.recv() {
              info!("Message {:?} Received", msg.clone());
              match msg {
                CollectorMessage::RequestSlotsForCenter(center) => {
                  let uri: Uri = format!(
                    "https://ttp.cbp.dhs.gov/schedulerapi/slots?orderBy=soonest&limit=5&locationId={}",
                    center
                  )
                  .parse()
                  .unwrap();

                  let resp = http_client.get(uri).await;
                  if let Ok(resp) = resp {
                    let data: Result<ScheduleSlots, _> =
                      serde_json::from_slice(&hyper::body::to_bytes(resp.into_body()).await.unwrap());
                    if let Ok(data) = data {
                      if data.len() > 0 {
                        if let Err(err) = tx.send(CollectorMessage::NotifyUsersOf(center, data)) {
                          warn!("Failed to send channel message {}", err);
                        }
                      } else {
                        info!("No slots avaliable for {}", center);
                      }
                    } else {
                      warn!("Failed to parse data: {}", data.unwrap_err())
                    }
                  } else {
                    warn!("Failed to contact endpoint")
                  }
                },
                CollectorMessage::NotifyUsersOf(center_id, slots) => {
                  let mut lock = MANAGER.lock().await;
                  let centers = lock.as_mut().unwrap().get_center_subscribers();
                  if slots.len() > 0 {
                    if let Some(users) = centers.get(&center_id) {
                      for user in users {
                        let user_data = lock.as_mut().unwrap().get_user_data(*user).await;
                        if user_data.is_ok() {
                          if let Some(user_data) = user_data.unwrap() {
                            for slot in slots.iter() {
                              let timeslot = NaiveDateTime::parse_from_str(&slot.start_timestamp, "%Y-%m-%dT%H:%M").unwrap();
                              let arrival = NaiveDate::from_ymd(2023, 1, 1);
                              let leave = NaiveDate::from_ymd(2023, 2, 1);
                              if timeslot.date() >= arrival && timeslot.date() <= leave {
                                if let Err(err) = bot
                                  .send_message(
                                    Recipient::Id(ChatId(user_data.chat_id)),
                                    CENTER_LUT[&slot.location_id].appointment_avaliable_msg(slot),
                                  )
                                  .parse_mode(ParseMode::MarkdownV2)
                                  .await
                                {
                                  warn!("Failed to send bot message {}", err);
                                }     
                              }
                            }
                          }
                        }
                      }
                    } else {
                      info!("Center {} has no subscribers", center_id);
                    }
                  } else {
                    warn!("Empty slot was messaged!");
                  }
                },
                CollectorMessage::Stop => return,
              }
            }

            thread::sleep(Duration::from_secs(1));
          }
        });
    });
  }
}

impl Drop for CenterDataCollectorTask {
  fn drop(&mut self) {
    info!("Stopping CenterDataCollectorTask...");
    if self.tx.send(CollectorMessage::Stop).is_err() {
      warn!("Failed to send stop command. Worker Thread may not exit nicely.")
    }
  }
}

impl Future for CenterDataCollectorTask {
  type Output = ();

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    if self.next_collection_time.is_none() || Instant::now() >= self.next_collection_time.unwrap() {
      info!("Starting work!");
      self.next_collection_time = Some(Instant::now() + Duration::from_secs(15));

      if let Ok(mut lock) = MANAGER.try_lock() {
        let centers = lock.as_mut().unwrap().get_center_subscribers();
        info!("Centers to check {:?}", centers);
        centers.keys().for_each(|&x| {
          if let Err(err) = self.tx.send(CollectorMessage::RequestSlotsForCenter(x)) {
            warn!("Failed to queue work message for center id {}: {}", x, err);
          }
        });
      } else {
        warn!("Failed to acquire lock, trying again shortly");
        self.next_collection_time = Some(Instant::now() + Duration::from_secs(1));
      }
    }

    let waker = cx.waker().clone();
    let when = self.next_collection_time.clone().unwrap();
    thread::spawn(move || {
      let dur = when - Instant::now();
      info!("Sleeping for {} seconds", dur.as_secs());
      thread::sleep(dur);
      waker.wake();
    });

    Poll::Pending
  }
}

# nexus-pls
A telegram bot to monitor avaliable appointments at NEXUS centers

## Required Environment Variables
- `REDIS_ADDR` Address to a non-authed redis server
- `TELOXIDE_TOKEN` Telegram Bot API Token

## Getting Started

```
# Run bot
REDIS_ADDR=... TELOXIDE_TOKEN=... cargo run
```

## Need More Centers?

Add them to [centers.toml](https://github.com/ChristopherJMiller/nexus-pls/blob/main/centers.toml) and make a PR.

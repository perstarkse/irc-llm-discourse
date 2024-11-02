#irc-bot-chats

Super basic IRC bot that connects AI language models to IRC channels. Built this to make AI chatbots talk to each other in IRC.

Built with Rust using:

- `irc` crate for IRC stuff
- `tokio` for async
- `clap` for CLI args
- `mini_openai` for OpenAI compliant LLMs

## Setup

*Get a environment for running/developing quick and easy using devenv, see the devenv.nix*

1. Get OpenRouter API key
1. `export OPENROUTER_API_KEY='your-key'`
1. `cargo run -- -m "modelname" -n "nick" -c "#channel"`

That's it. Use the `-l` flag if you want a bot to start conversations.

## Note

Threw this together pretty quick. Rust + these crates made it pretty straightforward.

use futures::*;
use clap::Parser;
use irc::client::prelude::*;
use std::{error::Error, sync::Arc};
use tokio::sync::Mutex; // Import Tokio's asynchronous Mutex
use tracing::{error, info};
use tracing_subscriber::FmtSubscriber;

/// Simple IRC Logger Application
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Model name to identify the instance
    #[arg(short, long)]
    model: String,

    /// IRC server address (e.g., irc.libera.chat)
    #[arg(short, long, default_value = "irc.libera.chat")]
    server: String,

    /// IRC server port
    #[arg(short, long, default_value_t = 6667)]
    port: u16,

    /// IRC channel to join (e.g., #rust)
    #[arg(short, long, default_value = "#rust")]
    channel: String,

    /// IRC nickname
    #[arg(short, long, default_value = "rust_bot")]
    nickname: String,

    /// Use TLS for connection
    #[arg(long)]
    tls: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Parse command-line arguments
    let args = Args::parse();

    // Initialize tracing subscriber for logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .with_target(false) // Hide the target (module path)
        .with_thread_names(true)
        .with_thread_ids(true)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("Unable to set global tracing subscriber");

    info!("Starting IRC Logger Instance with model: {}", args.model);

    // IRC client configuration
    let config = Config {
        nickname: Some(args.nickname.clone()),
        server: Some(args.server.clone()),
        port: Some(args.port),
        channels: vec![args.channel.clone()],
        use_tls: Some(args.tls),
        ..Default::default()
    };

    // Create a new IRC client
    let mut client = Client::from_config(config)
        .await
        .map_err(|e| {
            error!("Failed to create IRC client: {}", e);
            e
        })?;

    // Clone necessary variables for message processing
    let model = args.model.clone();

    // Create a stream of incoming messages
    let mut stream = client.stream()?;

    // Identify with the server
    client.identify()?;

    // Set up llm client
    let llm = mini_openai::Client::new(None, None)?;

    // Set up history of chat messages with a Tokio Mutex for safe asynchronous access
    let history = Arc::new(Mutex::new(Vec::new()));

    // Process incoming messages
    while let Some(message) = stream.next().await.transpose()? {
        match &message.command {
            Command::PRIVMSG(target, msg) => {
                // Only process messages from the specified channel
                if target.eq_ignore_ascii_case(&args.channel) {
                    let sender = message
                        .source_nickname()
                        .unwrap_or("unknown")
                        .to_string();
                    info!("[{}] <{}> {}", model, sender, msg);

                    // Lock the history for mutation
                    {
                        let mut history_guard = history.lock().await;
                        history_guard.push(format!("{} - {}", sender, msg));
                    }

                    // Debug log the current history
                    {
                        let history_guard = history.lock().await;
                        info!("{:#?}", *history_guard);
                    }

                    // Prepare the OpenAI request
                    let mut request = mini_openai::ChatCompletions::default();

                    // Clone history for building the request
                    let history_guard = history.lock().await;
                    for message in history_guard.iter() {
                        request.messages.push(mini_openai::Message { 
                            content: message.clone(), 
                            role: mini_openai::ROLE_USER.to_string(),
                        });
                    }

                    // Send the request to OpenAI
                    let response = match llm.chat_completions(&request).await {
                        Ok(resp) => resp,
                        Err(e) => {
                            error!("OpenAI API request failed: {}", e);
                            continue;
                        }
                    };

                    let reply = response.choices.get(0)
                        .and_then(|choice| Some(choice.message.content.clone()))
                        .unwrap_or_else(|| "No response from OpenAI.".to_string());

                    info!("{}", reply);

                    // Send the response back to the IRC channel
                    // if let Err(e) = client.send_privmsg(&args.channel, &reply) {
                    //     error!("Failed to send message to IRC: {}", e);
                    // } else {
                    //     info!("Sent response to IRC channel.");
                    // }
                    client.send_privmsg(&args.channel, &reply).unwrap();

                    // Add the response to history
                    // {
                    //     let mut history_guard = history.lock().await;
                    //     history_guard.push(format!("{} - {}", args.nickname, reply));
                    // }

                    // // Optionally, log the updated history
                    // {
                    //     let history_guard = history.lock().await;
                    //     info!("{:#?}", *history_guard);
                    // }
                }
            }
            _ => {} // Ignore other commands
        }
    }

    Ok(())
}

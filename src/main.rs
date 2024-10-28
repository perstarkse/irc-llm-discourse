use futures::*;
use clap::Parser;
use irc::client::prelude::*;
use std::{env, error::Error, sync::Arc};
use tokio::sync::Mutex; // Import Tokio's asynchronous Mutex
use tracing::{debug, error, info, Level};
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
    #[arg(short, long, default_value = "#chat_0098")]
    channel: String,

    /// IRC nickname
    #[arg(short, long, default_value = "bot")]
    nickname: String,

    /// Use TLS for connection
    #[arg(long)]
    tls: bool,

    #[arg(short,long, default_value = "false")]
    leader: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Parse command-line arguments
    let args = Args::parse();

    // Initialize tracing subscriber for logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
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

    let leader = args.leader.clone();

    // Create a stream of incoming messages
    let mut stream = client.stream()?;

    // Identify with the server
    client.identify()?;

    // Set up llm client
    let api_key = env::var("OPENROUTER_API_KEY").ok();
    let llm = mini_openai::Client::new(Some("https://openrouter.ai/api/v1".to_string()), api_key.clone())?;

    debug!("{:?}", api_key);

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
                debug!("<{}> {}", sender, msg);

                // Prepare the OpenAI request
                let mut request = mini_openai::ChatCompletions::default();

                request.model = "quid/lfm-40b:free".to_string();

                // Lock the history for reading
                let mut history_guard = history.lock().await;

                // Add the current message to the history
                history_guard.push(format!("{} - {}", sender, msg));

                // Add system message
                request.messages.push(mini_openai::Message { content: "only respond with unformatted text messages, no markdown or other formatting".to_string(), role:mini_openai::ROLE_USER.to_string()});

                // Clone history for building the request
                for message in history_guard.iter() {
                    request.messages.push(mini_openai::Message { 
                        content: message.clone(), 
                        role: mini_openai::ROLE_USER.to_string(),
                    });
                }
                
                // Drop the lock to avoid holding it during the API request
                drop(history_guard);

                if !leader && request.messages.len() < 3 { 
                    info!("Skipping first message");
                        continue 
                }

                // Send the request to OpenAI
                let response = match llm.chat_completions(&request).await {
                    Ok(resp) => resp,
                    Err(e) => {
                        error!("OpenAI API request failed: {}", e);
                        continue;
                    }
                };

                debug!("{:#?}", response);

                let reply = response.choices.get(0)
                    .and_then(|choice| Some(choice.message.content.clone()))
                    .unwrap_or_else(|| "No response from OpenAI.".to_string())
                    .replace("\n","")
                    .replace("`", "");

                debug!("{:#?}", response.choices.get(0));

                                // Send the response back to the IRC channel
                client.send_privmsg(&args.channel, &reply).unwrap();
                // let mut reply_chunks = Vec::new();
                // let mut chunk = String::new();
                // for word in reply.split_whitespace() {
                //     if chunk.len() + word.len() + 1 > 1512 {
                //         reply_chunks.push(chunk.clone());
                //         chunk.clear();
                //     }
                //     chunk.push_str(word);
                //     chunk.push(' ');
                // }
                
                // if !chunk.is_empty() {
                //     reply_chunks.push(chunk.clone());
                // }
                
                // for chunk in reply_chunks {
                                // Send the response back to the IRC channel
                    // client.send_privmsg(&args.channel, &chunk).unwrap();
                // }

                // Add the response to history
                let mut history_guard = history.lock().await;
                history_guard.push(format!("{} - {}", args.nickname, &reply));

                // Optionally, log the updated history
                debug!("{:#?}", *history_guard);
            }
        }
        _ => {} // Ignore other commands
    }
}

    Ok(())
}

use futures::*;
use clap::Parser;
use irc::client::prelude::*;
use std::{
    collections::HashMap,
    env,
    error::Error,
    sync::Arc,
};
use tokio::sync::{Mutex, mpsc};
use tokio::time::{self, Duration, Instant};
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

    #[arg(short, long, default_value = "false")]
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
        .with_thread_names(false)
        .with_thread_ids(false)
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
    let leader = args.leader;
    let nickname = args.nickname.clone();

    // Create a stream of incoming messages
    let mut stream = client.stream()?;

    // Identify with the server
    client.identify()?;

    // Set up LLM client
    let api_key = env::var("OPENROUTER_API_KEY").ok();
    let llm = mini_openai::Client::new_without_environment(
        "https://openrouter.ai/api/v1".to_string(),
        api_key.clone(),
    )?;

    // Set up history of chat messages with a Tokio Mutex for safe asynchronous access
    let history = Arc::new(Mutex::new(Vec::new()));

    // Set up a buffer for incoming messages
    // Key: sender nickname, Value: (Vec of messages, last received Instant)
    let message_buffer = Arc::new(Mutex::new(HashMap::<String, (Vec<String>, Instant)>::new()));

    // Set up a channel to send buffered messages for processing
    let (buffer_tx, mut buffer_rx) = mpsc::channel::<(String, String)>(100);

    // Clone variables to move into the background buffer handler task
    let buffer_clone = Arc::clone(&message_buffer);
    let history_clone = Arc::clone(&history);
    let channel_clone = args.channel.clone();
    let nickname_clone = nickname.clone();
    let leader_clone = leader;

    // Spawn a background task to handle buffered messages based on TTL
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_millis(100));
        loop {
            interval.tick().await;

            let mut buffer_guard = buffer_clone.lock().await;
            let now = Instant::now();
            let mut to_process = Vec::new();

            // Iterate over the buffer and collect senders whose last message was over 1 second ago
            for (sender, (msgs, last_instant)) in buffer_guard.iter_mut() {
                if now.duration_since(*last_instant) >= Duration::from_secs(1) {
                    // Combine messages into one
                    let combined_msg = msgs.join(" ");
                    to_process.push((sender.clone(), combined_msg.clone()));
                    // Clear the buffer for this sender
                    *msgs = Vec::new();
                }
            }

            // Remove senders with empty message buffers
            buffer_guard.retain(|_, (msgs, _)| !msgs.is_empty());

            drop(buffer_guard); // Release the lock before sending on channel

            for (sender, combined_msg) in to_process {
                if let Err(e) = buffer_tx.send((sender, combined_msg)).await {
                    error!("Failed to send buffered message to processor: {}", e);
                }
            }
        }
    });

    // Spawn a background task to process buffered messages
    let process_handle = tokio::spawn(async move {
        while let Some((sender, msg)) = buffer_rx.recv().await {
            debug!("<Buffered {}> {}", sender, msg);


            // Lock the history for reading
            let mut history_guard = history_clone.lock().await;

            // Add the current message to the history
            history_guard.push(format!("{} - {}", sender, msg));

            let mut messages = vec![]; 
            
            // Build the messages with the correct roles
            for message in history_guard.iter() {
                messages.push(mini_openai::Message { 
                    content: message.clone(), 
                    role: if sender == nickname_clone {
                        mini_openai::ROLE_ASSISTANT.to_string()
                    } else { 
                        mini_openai::ROLE_USER.to_string()
                    }
                });
            }

            // Prepare the OpenAI request
            let request = mini_openai::ChatCompletions {
                messages,
                model: model.to_string(),
                ..Default::default()
            };

            // Drop the lock to avoid holding it during the API request
            drop(history_guard);

            // Skip processing if not a leader and there are fewer than 2 messages in history
            if !leader_clone && request.messages.len() < 2 { 
                info!("Skipping first message");
                continue;
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

            // Extract and clean the reply from OpenAI's response
            let reply = response.choices.first()
                .map(|choice| choice.message.content.clone())
                .unwrap_or_else(|| "No response from OpenAI.".to_string())
                .replace('\n'," ")
                .replace('`', "");

            debug!("{:#?}", response.choices.first());

            // Split the reply into 500-character chunks to adhere to IRC limits
            let reply_chunks = split_into_chunks(&reply, 500);

            // Send each chunk with a small delay to handle IRC message limits
            for chunk in reply_chunks {
                if let Err(e) = client.send_privmsg(&channel_clone, &chunk) {
                    error!("Failed to send message chunk: {}", e);
                }
                // Introduce a small delay to prevent rapid sending
                time::sleep(Duration::from_millis(100)).await;
            }

            // Add the response to history
            let mut history_guard = history_clone.lock().await;
            history_guard.push(format!("{} - {}", nickname_clone, &reply));

            // Optionally, log the updated history
            debug!("{:#?}", *history_guard);
        }
    });

    // Function to split a string into chunks of max_size characters, preserving word boundaries
    fn split_into_chunks(text: &str, max_size: usize) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current_chunk = String::new();

        for word in text.split_whitespace() {
            // If a single word is longer than max_size, split the word itself
            if word.len() > max_size {
                if !current_chunk.is_empty() {
                    chunks.push(current_chunk.clone());
                    current_chunk.clear();
                }
                let word_chunks = word
                    .chars()
                    .collect::<Vec<_>>()
                    .chunks(max_size)
                    .map(|c| c.iter().collect::<String>())
                    .collect::<Vec<_>>();
                chunks.extend(word_chunks);
                continue;
            }

            if current_chunk.len() + word.len() + 1 > max_size && !current_chunk.is_empty() {
                    chunks.push(current_chunk.clone());
                    current_chunk.clear();
                }

            if !current_chunk.is_empty() {
                current_chunk.push(' ');
            }
            current_chunk.push_str(word);
        }

        if !current_chunk.is_empty() {
            chunks.push(current_chunk);
        }

        chunks
    }

    // Process incoming messages and buffer them
    while let Some(message) = stream.next().await.transpose()? {
           if let Command::PRIVMSG(target, msg) = &message.command {
                // Only process messages from the specified channel
                if target.eq_ignore_ascii_case(&args.channel) {
                    let sender = message
                        .source_nickname()
                        .unwrap_or("unknown")
                        .to_string();
                    debug!("<{}> {}", sender, msg);

                    // Add the message to the buffer with the current timestamp
                    let mut buffer_guard = message_buffer.lock().await;
                    let entry = buffer_guard.entry(sender.clone()).or_insert((Vec::new(), Instant::now()));
                    entry.0.push(msg.clone());
                    entry.1 = Instant::now(); // Update the last received time
                }
            }
    }

    // Await the processing task (this point is typically never reached)
    process_handle.await?;

    Ok(())
}


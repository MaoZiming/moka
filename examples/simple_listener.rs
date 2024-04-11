use moka::sync::Cache;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    let ttl = 60; // Time-to-live for cache entries in seconds
    let cache = Cache::builder()
        .max_capacity(100)
        .time_to_live(Duration::from_secs(ttl))
        .eviction_listener(|key, value, cause| {
            println!("Evicted ({key:?},{value:?}) because {cause:?}");
        })
        .build();

    let listener = TcpListener::bind("127.0.0.1:8080").await.unwrap();
    println!("Listening on 127.0.0.1:8080");

    loop {
        let (mut socket, _) = listener.accept().await.unwrap();
        let cache = cache.clone();

        tokio::spawn(async move {
            let mut buf = [0; 1024];

            // In a loop, read data from the socket and write the data back.
            loop {
                let n = match socket.read(&mut buf).await {
                    Ok(n) if n == 0 => return,
                    Ok(n) => n,
                    Err(e) => {
                        println!("Failed to read from socket; err = {:?}", e);
                        return;
                    }
                };

                let msg = match std::str::from_utf8(&buf[..n]) {
                    Ok(v) => v,
                    Err(e) => {
                        println!("Could not interpret message as UTF-8; err = {:?}", e);
                        return;
                    }
                };

                let parts: Vec<&str> = msg.splitn(2, ' ').collect();
                if parts.len() != 2 {
                    println!("Invalid message format. Expected format: <key> <value>");
                    continue;
                }

                let key = parts[0].trim().to_string();
                let value = parts[1].trim().to_string();

                cache.insert(key.clone(), value.clone());
                println!("Inserted {:?}-{:?} pair into cache.", key, value);

                if let Err(e) = socket.write_all(b"Inserted\n").await {
                    println!("Failed to write to socket; err = {:?}", e);
                    return;
                }
            }
        });
    }
}

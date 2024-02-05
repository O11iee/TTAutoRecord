#![allow(deprecated)]
use reqwest::{self, Client, header};
use tokio::sync::Mutex;
use std::sync::Arc;
use std::time::Duration;
use chrono::Local;
use once_cell::sync::Lazy;
use regex::Regex;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::error::Error;
use tokio::time::timeout;
use config::{Config, File, FileFormat};
use std::process::Stdio;
use std::collections::HashMap;
use std::time::SystemTime;
use std::fs::metadata;
use std::str::FromStr;


static IPHONE_USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 13_2_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/13.0.3 Mobile/15E148 Safari/604.1";
static FFMPEG_PATH: &str = "ffmpeg.exe";
static LOCK_DIRECTORY: &str = "../lock_files";
static RE_M3U8: Lazy<Regex> = Lazy::new(|| Regex::new(r#""hls_pull_url":\s*"([^"]+)"#).unwrap());
static RE_FLV: Lazy<Regex> = Lazy::new(|| Regex::new(r#""rtmp_pull_url":\s*"([^"]+)"#).unwrap());


static RE_USERNAME: Lazy<Regex> = Lazy::new(|| Regex::new(r#""display_id":"([^"]+)""#).unwrap());

enum FetchResult {
    UrlAndUsername(String, String),
    NotFound,
    LiveEnded,
    UsernameMissing,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
   // hide_console_window();
    let mut config = Config::default();
    config.merge(File::new("./config/config.toml", FileFormat::Toml))?;
    let chunks: usize = config.get("chunks")?;
    let duration: u64 = config.get("duration")?;

    clear_lock_files_directory().await?;
    fs::create_dir_all(LOCK_DIRECTORY)?;

    let client = reqwest::Client::builder()
        .user_agent(IPHONE_USER_AGENT)
        .build()?;

        loop {
            let urls = read_room_ids_from_file()?;
            let state = Arc::new(Mutex::new(HashMap::new()));
            for chunk in urls.chunks(chunks) {
                let futures: Vec<_> = chunk.iter().map(|url| {
                    download_video(&client, url, state.clone())
                }).collect();
                futures::future::join_all(futures).await;
                tokio::time::sleep(Duration::from_secs(duration)).await;
    
            }
        }
}


fn read_room_ids_from_file() -> Result<Vec<String>, Box<dyn Error>> {
    let file = fs::File::open("./config/lists/room_ids.txt")?;
    let reader = BufReader::new(file);
    let room_ids: Vec<String> = reader.lines()
        .filter_map(Result::ok)
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('=').collect();
            if parts.len() == 2 {
                Some(parts[1].trim().to_string())
            } else {
                None
            }
        })
        .collect();
    Ok(room_ids)
}

async fn clear_lock_files_directory() -> Result<(), Box<dyn std::error::Error>> {
    if Path::new(LOCK_DIRECTORY).exists() {
        let mut dir = tokio::fs::read_dir(LOCK_DIRECTORY).await?;
        while let Some(entry) = dir.next_entry().await? {
            if entry.path().is_file() {
                tokio::fs::remove_file(entry.path()).await?;
            }
        }
    }
    Ok(())
}

async fn fetch_room_info_and_extract(client: &Client, room_id: &str, state: Arc<Mutex<HashMap<String, String>>>) -> Result<FetchResult, Box<dyn Error>> {
    let _current_time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let url = format!("https://webcast.tiktok.com/webcast/room/info/?aid=1988&room_id={}", room_id);

    // Creating custom headers
    let mut custom_headers = header::HeaderMap::new();
    custom_headers.insert(header::ACCEPT, "text/html,application/xhtml+xml,application/xml;q=0.9,image/jxl,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7".parse().unwrap());
    custom_headers.insert(header::ACCEPT_LANGUAGE, "en-GB,en-US;q=0.9,en;q=0.8".parse().unwrap());
    custom_headers.insert(header::CACHE_CONTROL, "max-age=0".parse().unwrap());
    custom_headers.insert(header::DNT, "1".parse().unwrap());

    // Inserting custom headers
    custom_headers.insert(header::HeaderName::from_str("sec-ch-ua").unwrap(), "\"Chromium\";v=\"117\", \"Not;A=Brand\";v=\"8\"".parse().unwrap());
    custom_headers.insert(header::HeaderName::from_str("sec-ch-ua-mobile").unwrap(), "?0".parse().unwrap());
    custom_headers.insert(header::HeaderName::from_str("sec-ch-ua-platform").unwrap(), "\"Windows\"".parse().unwrap());
    custom_headers.insert(header::HeaderName::from_str("sec-fetch-dest").unwrap(), "document".parse().unwrap());
    custom_headers.insert(header::HeaderName::from_str("sec-fetch-mode").unwrap(), "navigate".parse().unwrap());
    custom_headers.insert(header::HeaderName::from_str("sec-fetch-site").unwrap(), "none".parse().unwrap());
    custom_headers.insert(header::HeaderName::from_str("sec-fetch-user").unwrap(), "?1".parse().unwrap());

    custom_headers.insert(header::UPGRADE_INSECURE_REQUESTS, "1".parse().unwrap());
    custom_headers.insert(header::USER_AGENT, "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/117.0.0.0 Safari/537.36".parse().unwrap());

    let response = timeout(Duration::from_secs(10), client.get(&url).headers(custom_headers).send()).await??;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        //println!("{}: Room not found: {}", Local::now().format("%Y-%m-%d %H:%M:%S"), room_id);
        return Ok(FetchResult::NotFound);
    }

    let content = response.bytes().await?;
    let text = String::from_utf8_lossy(&content);

    let mut state_lock = state.lock().await;

    if text.contains("\"status\":2") {
        //println!("{}: Room live: {}", Local::now().format("%Y-%m-%d %H:%M:%S"), room_id);

        if let Some(username_cap) = RE_USERNAME.captures(&text) {
            let username = username_cap[1].to_string();
            //println!("Username found: {}", username);

            let m3u8_url = RE_M3U8.captures(&text).and_then(|cap| cap.get(1)).map(|m| m.as_str().to_string());
            let flv_url = RE_FLV.captures(&text).and_then(|cap| cap.get(1)).map(|m| m.as_str().to_string());

            //println!("M3U8 URL: {:?}", m3u8_url);
            //println!("FLV URL: {:?}", flv_url);

            if let Some(url) = m3u8_url.or(flv_url) {
                //println!("Selected URL: {}", url);
                state_lock.insert(room_id.to_string(), if url.ends_with(".m3u8") { "m3u8" } else { "flv" }.to_string());
                return Ok(FetchResult::UrlAndUsername(url, username));
            } else {
                //println!("No valid URL found");
                return Ok(FetchResult::LiveEnded);
            }
        } else {
            //println!("Username not found for room: {}", room_id);
            return Ok(FetchResult::UsernameMissing);
        }
    } else {
        //println!("Room is not live: {}", room_id);
        return Ok(FetchResult::LiveEnded);
    }
}



fn extract_stream_id(url: &str) -> Option<String> {
    let re = Regex::new(r"stream-(\d+)_").unwrap();
    re.captures(url).and_then(|cap| cap.get(1).map(|m| m.as_str().to_string()))
}

async fn download_video(client: &reqwest::Client, room_id: &str, state: Arc<Mutex<HashMap<String, String>>>) -> Result<(), Box<dyn std::error::Error>> {
    let fetch_result = fetch_room_info_and_extract(client, room_id, state.clone()).await?;

    match fetch_result {
        FetchResult::UrlAndUsername(mut url, username) => {
            
            let lock_file_path = format!("{}/{}.lock", LOCK_DIRECTORY, username);
            if Path::new(&lock_file_path).exists() {
                return Ok(());
            }
            fs::File::create(&lock_file_path)?;
            println!("{}: Downloading video for {}", Local::now().format("%Y-%m-%d %H:%M:%S"), username);

            let datetime = Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
            let date: String = Local::now().format("%Y-%m-%d").to_string();
            let stream_id = extract_stream_id(&url).unwrap_or_else(|| "unknown".to_string());
            let video_folder = format!("../segments/{}/segments/", username);
            fs::create_dir_all(&video_folder)?; // Create the user-specific folder if it doesn't exist

            let video_path = format!("{}/{}_{}_{}.mp4", video_folder, username, stream_id, datetime);
            let output_path = format!("../videos/{}_{}_{}.mp4", username, stream_id, date);
            let output_folder = format!("../videos/");

            //println!("{}: {} is live", Local::now().format("%Y-%m-%d %H:%M:%S"), username);

            url = url.replace("\\u0026", "&");

            //download video
            tokio::spawn(async move {
                let mut ffmpeg = tokio::process::Command::new(FFMPEG_PATH)
                    .arg("-i")
                    .arg(&url)
                    .arg("-reconnect")          // Enable reconnect
                    .arg("1")
                    .arg("-reconnect_at_eof")   // Reconnect at end of file
                    .arg("1")
                    .arg("-reconnect_streamed") // Reconnect streamed
                    .arg("1")
                    .arg("-reconnect_delay_max") // Max reconnect delay in seconds
                    .arg("1")
                    .arg("-timeout")            // Set timeout to 30 seconds
                    .arg("30")
                    .arg("-c")
                    .arg("copy")
                    .arg("-bsf:a")
                    .arg("aac_adtstoasc")
                    .arg(&video_path)
                    .stdout(Stdio::null()) // Redirect stdout to null
                    .stderr(Stdio::null()) // Redirect stderr to null
                    .spawn()
                    .expect("Failed to start ffmpeg process");

                    ffmpeg.wait().await.expect("Failed to wait for ffmpeg process");

            let mut video_files_to_concat: Vec<String> = Vec::new();

            let concat_file_name = format!("concat-{}.txt", stream_id); // Define concat file name

            let mut dir = tokio::fs::read_dir(&video_folder).await.expect("Failed to read video folder");
            while let Some(entry) = dir.next_entry().await.expect("Failed to read video folder") {
                if entry.path().is_file() {
                    let file_name = entry.file_name();
                    let file_name = file_name.to_str().unwrap();
        
                    // Skip adding concat file to the list
                    if file_name.contains(&stream_id) && file_name != concat_file_name {
                        video_files_to_concat.push(file_name.to_string());
                    }
                }
            }

            // wait 1 seconds for segment to be written
            tokio::time::sleep(Duration::from_secs(1)).await;

            video_files_to_concat.sort();

            let mut concat_file = fs::File::create(format!("{}concat-{}.txt", video_folder, stream_id)).expect("Failed to create concat file");
            for video_file in &video_files_to_concat {
                Write::write_all(&mut concat_file, format!("file '{}'\n", video_file).as_bytes()).expect("Failed to write to concat file");
            }

            // wait 1 seconds for concat file to be written
            tokio::time::sleep(Duration::from_secs(1)).await;

            if video_files_to_concat.len() < 2 {
                // If there's only one video, copy and rename it
                let copied_path = format!("{}/{}_{}_{}.mp4", output_folder, username, stream_id, date);
                if let Err(_copy_error) = fs::copy(&video_path, &copied_path) {
                    //println!("Failed to copy video to main folder: {}", copy_error);
                    tokio::fs::remove_file(&lock_file_path).await.expect("Failed to remove lock file");
                }
            } else if video_files_to_concat.len() >= 2 {
                println!("{}: Concatenating LIVE segments: {}", Local::now().format("%Y-%m-%d %H:%M:%S"), username);
                let mut ffmpeg = tokio::process::Command::new(FFMPEG_PATH)
                    .arg("-f")
                    .arg("concat")
                    .arg("-safe")
                    .arg("0")
                    .arg("-i")
                    .arg(format!("{}concat-{}.txt", video_folder, stream_id))
                    .arg("-c")
                    .arg("copy")
                    .arg("-y")
                    .arg(&output_path)
                    .stdout(Stdio::null()) // Redirect stdout to null
                    .stderr(Stdio::null()) // Redirect stderr to null
                    .spawn()
                    .expect("Failed to start ffmpeg process");

                ffmpeg.wait().await.expect("Failed to wait for ffmpeg process");

                let concat_file_path = format!("{}/concat.txt", video_folder);
                if Path::new(&concat_file_path).exists() {
                    fs::remove_file(&concat_file_path).expect("Failed to remove concat file");
                    //println!("{}: Video Concatenated: {}", Local::now().format("%Y-%m-%d %H:%M:%S"), username);
                }

            }

            if Path::new(&lock_file_path).exists() {
                tokio::fs::remove_file(&lock_file_path).await.expect("Failed to remove lock file");
            }
        });
        },
        FetchResult::NotFound => {
            println!("{}: Error retrieving data for {} - You are most likely rate limited! Tweak your config.toml", Local::now().format("%Y-%m-%d %H:%M:%S"), room_id);
        },
        FetchResult::LiveEnded => {
        },
        FetchResult::UsernameMissing => {
            println!("{}: Username missing for {}", Local::now().format("%Y-%m-%d %H:%M:%S"), room_id);
        },
    }

    Ok(())
}
use actix_cors::Cors;
use actix_web::{get, post, put, web, App, HttpResponse, HttpServer, Responder};
use actix_web_actors::ws;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use mongodb::{
    bson::{doc, oid::ObjectId},
    options::ClientOptions,
    Client, Collection,
};
use redis::{AsyncCommands, Client as RedisClient};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use uuid::Uuid;

// Import necessary actix traits
use actix::{Actor, ActorContext, AsyncContext, Handler, Message as ActixMessage, StreamHandler};

// Custom message type for WebSocket communication
#[derive(ActixMessage)]
#[rtype(result = "()")]
struct WsMessage(String);

// WebSocket connection state
struct WebSocketSession {
    id: Uuid,
    hb: Instant,
}

impl WebSocketSession {
    fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            hb: Instant::now(),
        }
    }

    // Heartbeat to check if connection is alive
    fn hb(&self) -> Duration {
        Instant::now().duration_since(self.hb)
    }
}

impl Actor for WebSocketSession {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        // Start heartbeat
        ctx.run_interval(Duration::from_secs(10), |act, ctx| {
            if act.hb() > Duration::from_secs(30) {
                println!("WebSocket client failed heartbeat, disconnecting!");
                ctx.stop();
            } else {
                ctx.ping(b"");
            }
        });
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WebSocketSession {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Ping(msg)) => {
                self.hb = Instant::now();
                ctx.pong(&msg);
            }
            Ok(ws::Message::Pong(_)) => {
                self.hb = Instant::now();
            }
            Ok(ws::Message::Text(text)) => {
                // Handle incoming WebSocket messages
                println!("Received WebSocket message: {}", text);
                // Echo back for demonstration
                ctx.text(text);
            }
            Ok(ws::Message::Close(reason)) => {
                ctx.close(reason);
                ctx.stop();
            }
            _ => (),
        }
    }
}

// Handler for our custom WsMessage
impl Handler<WsMessage> for WebSocketSession {
    type Result = ();

    fn handle(&mut self, msg: WsMessage, ctx: &mut Self::Context) {
        ctx.text(msg.0);
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ChatMessage {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
    id: Option<ObjectId>,
    text: String,
    author: String,
    timestamp: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct UpdateMessage {
    text: String,
}

// Global state for WebSocket connections
type WebSocketSessions = Arc<RwLock<HashMap<Uuid, actix::Addr<WebSocketSession>>>>;

struct AppState {
    mongo: Collection<ChatMessage>,
    redis_client: Arc<RedisClient>,
    ws_sessions: WebSocketSessions,
}

/// WebSocket handler
#[get("/ws")]
async fn websocket(
    req: actix_web::HttpRequest,
    stream: web::Payload,
    data: web::Data<AppState>,
) -> Result<HttpResponse, actix_web::Error> {
    let session = WebSocketSession::new();
    let session_id = session.id;

    let resp = ws::WsResponseBuilder::new(session, &req, stream).start_with_addr()?;

    let (addr, response) = resp;

    // Store the session for broadcasting
    {
        let mut sessions_write = data.ws_sessions.write().await;
        sessions_write.insert(session_id, addr.clone());
        println!("New WebSocket connection: {}", session_id);
    }

    // Remove session when connection closes
    let sessions_clone = data.ws_sessions.clone();

    actix_rt::spawn(async move {
        // We'll handle cleanup through the heartbeat timeout
        // The session will be automatically removed when the actor stops
        println!("WebSocket session {} started", session_id);

        // Wait for a long time (session will be cleaned up by heartbeat)
        tokio::time::sleep(Duration::from_secs(3600)).await;

        // If we reach here, remove the session
        let mut sessions_write = sessions_clone.write().await;
        sessions_write.remove(&session_id);
        println!("WebSocket session {} cleaned up", session_id);
    });

    Ok(response)
}

/// Broadcast message to all WebSocket clients
async fn broadcast_to_websockets(sessions: &WebSocketSessions, message: &str) {
    let sessions_read = sessions.read().await;
    let mut disconnected_sessions = Vec::new();

    for (id, addr) in sessions_read.iter() {
        // Send message to WebSocket session using our custom message type
        match addr.try_send(WsMessage(message.to_string())) {
            Ok(_) => {
                // Message sent successfully
            }
            Err(_) => {
                // Session might be disconnected, mark for cleanup
                disconnected_sessions.push(*id);
            }
        }
    }

    // Clean up disconnected sessions
    if !disconnected_sessions.is_empty() {
        drop(sessions_read);
        let mut sessions_write = sessions.write().await;
        for id in disconnected_sessions {
            sessions_write.remove(&id);
            println!("Cleaned up disconnected WebSocket session: {}", id);
        }
    }
}

/// SSE endpoint (for clients that prefer SSE)
#[get("/events")]
async fn sse_events(data: web::Data<AppState>) -> impl Responder {
    let (mut tx, rx) = futures::channel::mpsc::channel::<String>(100);
    let _redis_client = data.redis_client.clone();
    let ws_sessions = data.ws_sessions.clone();

    actix_rt::spawn(async move {
        // For pubsub, we need to use the deprecated method temporarily
        let pubsub_client = RedisClient::open("redis://127.0.0.1/").unwrap();
        let pubsub_conn = match pubsub_client.get_async_connection().await {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("Failed to create pubsub Redis connection: {}", e);
                return;
            }
        };

        let mut pubsub = pubsub_conn.into_pubsub();

        if let Err(e) = pubsub.subscribe("updates").await {
            eprintln!("Failed to subscribe to updates: {}", e);
            return;
        }

        while let Some(msg) = pubsub.on_message().next().await {
            let payload: String = match msg.get_payload() {
                Ok(payload) => payload,
                Err(e) => {
                    eprintln!("Failed to get payload: {}", e);
                    continue;
                }
            };

            // Send to SSE clients
            if tx.send(payload.clone()).await.is_err() {
                break;
            }

            // Also broadcast to WebSocket clients
            broadcast_to_websockets(&ws_sessions, &payload).await;
        }
    });

    let stream = rx.map(|msg| Ok::<_, actix_web::Error>(Bytes::from(format!("data: {}\n\n", msg))));

    HttpResponse::Ok()
        .insert_header(("Content-Type", "text/event-stream"))
        .insert_header(("Cache-Control", "no-cache"))
        .insert_header(("Access-Control-Allow-Origin", "*"))
        .streaming(stream)
}

/// POST new message
#[post("/publish")]
async fn publish(state: web::Data<AppState>, payload: web::Json<ChatMessage>) -> impl Responder {
    let mut msg = payload.into_inner();

    // Add timestamp
    msg.timestamp = Some(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    );

    // Insert into MongoDB
    let insert_result = match state.mongo.insert_one(&msg).await {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Failed to insert into MongoDB: {}", e);
            return HttpResponse::InternalServerError().body("Failed to save message");
        }
    };
    msg.id = insert_result.inserted_id.as_object_id();

    // Publish new message using multiplexed connection
    let mut conn = match state.redis_client.get_multiplexed_async_connection().await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("Failed to connect to Redis: {}", e);
            return HttpResponse::InternalServerError().body("Failed to connect to Redis");
        }
    };

    let json_msg = match serde_json::to_string(&msg) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("Failed to serialize message: {}", e);
            return HttpResponse::InternalServerError().body("Failed to serialize message");
        }
    };

    // Explicitly type the result
    let result: Result<(), redis::RedisError> = conn.publish("updates", &json_msg).await;
    if let Err(e) = result {
        eprintln!("Failed to publish message: {}", e);
        return HttpResponse::InternalServerError().body("Failed to publish message");
    }

    HttpResponse::Ok().json(msg)
}

/// PUT edit message
#[put("/edit/{id}")]
async fn edit_message(
    state: web::Data<AppState>,
    path: web::Path<String>,
    payload: web::Json<UpdateMessage>,
) -> impl Responder {
    let id_str = path.into_inner();

    // Better error handling for ObjectId parsing
    let id = match ObjectId::parse_str(&id_str) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("Invalid ObjectId '{}': {}", id_str, e);
            return HttpResponse::BadRequest().body(format!("Invalid message ID: {}", id_str));
        }
    };

    let filter = doc! { "_id": &id };
    let update = doc! { "$set": { "text": &payload.text } };

    let update_result = match state.mongo.update_one(filter.clone(), update).await {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Failed to update message: {}", e);
            return HttpResponse::InternalServerError().body("Failed to update message");
        }
    };

    if update_result.matched_count == 0 {
        return HttpResponse::NotFound().body("Message not found");
    }

    // Fetch updated message
    let mut updated = match state.mongo.find_one(filter).await {
        Ok(Some(msg)) => msg,
        Ok(None) => {
            return HttpResponse::NotFound().body("Message not found after update");
        }
        Err(e) => {
            eprintln!("Failed to fetch updated message: {}", e);
            return HttpResponse::InternalServerError().body("Failed to fetch updated message");
        }
    };

    // Update timestamp
    updated.timestamp = Some(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    );

    // Publish updated message
    let mut conn = match state.redis_client.get_multiplexed_async_connection().await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("Failed to connect to Redis: {}", e);
            return HttpResponse::InternalServerError().body("Failed to connect to Redis");
        }
    };

    let json_msg = match serde_json::to_string(&updated) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("Failed to serialize message: {}", e);
            return HttpResponse::InternalServerError().body("Failed to serialize message");
        }
    };

    // Explicitly type the result
    let result: Result<(), redis::RedisError> = conn.publish("updates", &json_msg).await;
    if let Err(e) = result {
        eprintln!("Failed to publish update: {}", e);
        return HttpResponse::InternalServerError().body("Failed to publish update");
    }

    HttpResponse::Ok().json(updated)
}

/// Get all messages (for new WebSocket clients)
#[get("/messages")]
async fn get_messages(state: web::Data<AppState>) -> impl Responder {
    match state.mongo.find(doc! {}).await {
        Ok(mut cursor) => {
            let mut messages = Vec::new();
            while let Some(Ok(message)) = cursor.next().await {
                messages.push(message);
            }
            HttpResponse::Ok().json(messages)
        }
        Err(e) => {
            eprintln!("Failed to fetch messages: {}", e);
            HttpResponse::InternalServerError().body("Failed to fetch messages")
        }
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // MongoDB setup
    let client_options = match ClientOptions::parse("mongodb://localhost:27017").await {
        Ok(options) => options,
        Err(e) => {
            eprintln!("Failed to parse MongoDB connection string: {}", e);
            std::process::exit(1);
        }
    };

    let client = match Client::with_options(client_options) {
        Ok(client) => client,
        Err(e) => {
            eprintln!("Failed to create MongoDB client: {}", e);
            std::process::exit(1);
        }
    };

    let db = client.database("school");
    let coll = db.collection::<ChatMessage>("messages");

    // Redis setup
    let redis_client = match RedisClient::open("redis://127.0.0.1/") {
        Ok(client) => client,
        Err(e) => {
            eprintln!("Failed to create Redis client: {}", e);
            std::process::exit(1);
        }
    };

    // Test Redis connection
    match redis_client.get_multiplexed_async_connection().await {
        Ok(_) => println!("✅ Connected to Redis successfully"),
        Err(e) => {
            eprintln!("❌ Failed to connect to Redis: {}", e);
            std::process::exit(1);
        }
    }

    let state = web::Data::new(AppState {
        mongo: coll,
        redis_client: Arc::new(redis_client),
        ws_sessions: Arc::new(RwLock::new(HashMap::new())),
    });

    println!("🚀 Server running on http://127.0.0.1:4877");
    println!("📡 SSE endpoint: http://127.0.0.1:4877/events");
    println!("🔌 WebSocket endpoint: ws://127.0.0.1:4877/ws");
    println!("📨 Messages endpoint: http://127.0.0.1:4877/messages");

    HttpServer::new(move || {
        App::new()
            .wrap(
                Cors::default()
                    .allow_any_origin()
                    .allow_any_method()
                    .allow_any_header()
                    .supports_credentials(),
            )
            .app_data(state.clone())
            .service(sse_events)
            .service(websocket)
            .service(publish)
            .service(edit_message)
            .service(get_messages)
    })
    .bind(("127.0.0.1", 4877))?
    .run()
    .await
}

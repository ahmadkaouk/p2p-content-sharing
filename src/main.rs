#![feature(once_cell)]

use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    error::Error,
    hash::{Hash, Hasher},
    lazy::SyncOnceCell,
    str::FromStr,
};

use libp2p::{
    core::upgrade,
    futures::StreamExt,
    gossipsub::{
        self, Gossipsub, GossipsubEvent, GossipsubMessage, IdentTopic, MessageAuthenticity,
        MessageId,
    },
    identity,
    kad::{store::MemoryStore, Kademlia, KademliaConfig, KademliaEvent},
    mdns::{Mdns, MdnsEvent},
    mplex, noise,
    swarm::{NetworkBehaviourEventProcess, Swarm, SwarmBuilder},
    tcp::TokioTcpConfig,
    Multiaddr, NetworkBehaviour, PeerId, Transport,
};

use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::{self, AsyncBufReadExt},
    sync::mpsc,
};

const STORAGE_FILE_PATH: &str = "./contents.json";

static PEER_ID: SyncOnceCell<PeerId> = SyncOnceCell::new();

#[derive(NetworkBehaviour)]
#[behaviour(event_process = true)]
struct CustomBehaviour {
    gossipsub: Gossipsub,
    kademlia: Kademlia<MemoryStore>,
    mdns: Mdns,
    #[behaviour(ignore)]
    response_sender: mpsc::UnboundedSender<ListResponse>,
}

const BOOTNODES: [&str; 4] = [
    "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
    "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
    "QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb",
    "QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
];

type Contents = Vec<Content>;

#[derive(Debug, Serialize, Deserialize)]
struct Content {
    id: usize,
    author: String,
    content: String,
    public: bool,
}

#[derive(Debug, Serialize, Deserialize)]
enum ListMode {
    All,
    One(String),
}

#[derive(Debug, Serialize, Deserialize)]
struct ListRequest {
    mode: ListMode,
}

#[derive(Debug, Serialize, Deserialize)]
struct ListResponse {
    mode: ListMode,
    data: Contents,
    receiver: String,
}

enum EventType {
    Response(ListResponse),
    Input(String),
}

impl NetworkBehaviourEventProcess<GossipsubEvent> for CustomBehaviour {
    // Called when `gossipsub` produces an event.
    fn inject_event(&mut self, message: GossipsubEvent) {
        if let GossipsubEvent::Message { message: msg, .. } = message {
            if let Ok(resp) = serde_json::from_slice::<ListResponse>(&msg.data) {
                if resp.receiver == PEER_ID.get().unwrap().to_string() {
                    println!("Response from {}:", msg.source.unwrap());
                    resp.data.iter().for_each(|r| println!("{:?}", r));
                }
            } else if let Ok(req) = serde_json::from_slice::<ListRequest>(&msg.data) {
                match req.mode {
                    ListMode::All => {
                        println!("Received ALL req: {:?} from {:?}", req, msg.source);
                        respond_with_public_contents(
                            self.response_sender.clone(),
                            msg.source.unwrap().to_string(),
                        );
                    }
                    ListMode::One(ref peer_id) => {
                        if peer_id == &PEER_ID.get().unwrap().to_string() {
                            println!("Received req: {:?} from {:?}", req, msg.source);
                            respond_with_public_contents(
                                self.response_sender.clone(),
                                msg.source.unwrap().to_string(),
                            );
                        }
                    }
                }
            }
        }
    }
}

fn respond_with_public_contents(sender: mpsc::UnboundedSender<ListResponse>, receiver: String) {
    tokio::spawn(async move {
        match read_local_contents().await {
            Ok(contents) => {
                let resp = ListResponse {
                    mode: ListMode::All,
                    receiver,
                    data: contents.into_iter().filter(|r| r.public).collect(),
                };
                if let Err(e) = sender.send(resp) {
                    println!("error sending response via channel, {}", e);
                }
            }
            Err(e) => println!("error fetching local contents to answer ALL request, {}", e),
        }
    });
}

impl NetworkBehaviourEventProcess<MdnsEvent> for CustomBehaviour {
    // Called when `mdns` produces an event.
    fn inject_event(&mut self, event: MdnsEvent) {
        if let MdnsEvent::Discovered(list) = event {
            for (peer_id, multiaddr) in list {
                self.kademlia.add_address(&peer_id, multiaddr);
            }
        }
    }
}

impl NetworkBehaviourEventProcess<KademliaEvent> for CustomBehaviour {
    fn inject_event(&mut self, _message: KademliaEvent) {}
}

type Result<T> = std::result::Result<T, Box<dyn Error>>;
#[tokio::main]
async fn main() -> Result<()> {
    // Generate a peer ID
    let local_keys = identity::Keypair::generate_ed25519();
    PEER_ID.get_or_init(|| PeerId::from(local_keys.public()));
    println!("Peer ID: {:?}", PEER_ID.get().unwrap());

    let (response_sender, mut response_rcv) = mpsc::unbounded_channel();

    // Create a keypair for authenticated encryption of the transport.
    let noise_keys = noise::Keypair::<noise::X25519Spec>::new()
        .into_authentic(&local_keys)
        .expect("Signing libp2p-noise static DH keypair failed.");

    // Create a tokio-based TCP transport, use noise for authenticated
    // encryption and Mplex for multiplexing of substreams on a TCP stream.
    let transport = TokioTcpConfig::new()
        .nodelay(true)
        .upgrade(upgrade::Version::V1)
        .authenticate(noise::NoiseConfig::xx(noise_keys).into_authenticated())
        .multiplex(mplex::MplexConfig::new())
        .boxed();

    // Create a gossipsub topic
    let topic = IdentTopic::new("content");

    // Create a Swarm to manage peers and events
    let mut swarm = {
        // Use the hash of messages for content-addressing
        let msg_id = |message: &GossipsubMessage| {
            let mut s = DefaultHasher::new();
            message.data.hash(&mut s);
            MessageId::from(s.finish().to_string())
        };

        // Set custom parameters for gossipsub
        let gossipsub_config = gossipsub::GossipsubConfigBuilder::default()
            .message_id_fn(msg_id)
            .build()?;

        // build a gossipsub network behaviour
        let mut gossipsub: gossipsub::Gossipsub =
            gossipsub::Gossipsub::new(MessageAuthenticity::Signed(local_keys), gossipsub_config)
                .expect("Correct configuration");

        // Create a Kademlia behaviour.
        let mut kademlia = Kademlia::with_config(
            *PEER_ID.get().unwrap(),
            MemoryStore::new(*PEER_ID.get().unwrap()),
            KademliaConfig::default(),
        );

        // Add the bootnodes to the local routing table. `libp2p-dns` built
        // into the `transport` resolves the `dnsaddr` when Kademlia tries
        // to dial these nodes.
        let bootaddr = Multiaddr::from_str("/dnsaddr/bootstrap.libp2p.io")?;
        for peer in &BOOTNODES {
            kademlia.add_address(&PeerId::from_str(peer)?, bootaddr.clone());
        }

        // subscribe the gossipsub to topic
        gossipsub.subscribe(&topic)?;
        let behaviour = CustomBehaviour {
            gossipsub,
            kademlia,
            mdns: Mdns::new(Default::default())
                .await
                .expect("Cant create mdns"),
            response_sender,
        };
        SwarmBuilder::new(transport, behaviour, *PEER_ID.get().unwrap())
            .executor(Box::new(|fut| {
                tokio::spawn(fut);
            }))
            .build()
    };

    // Read full lines from stdin
    let mut stdin = io::BufReader::new(io::stdin()).lines();

    // Listen on all interfaces and whatever port the OS assigns
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    // loop infinitly
    loop {
        let evt = {
            tokio::select! {
                line = stdin.next_line() => Some(EventType::Input(line.expect("Cant get line").expect("Cant read line"))),
                response = response_rcv.recv() => Some(EventType::Response(response.expect("Response error"))),
                event = swarm.select_next_some() => {
                    println!("Unhandled swarm event: {:?}", event);
                    None
                },
            }
        };

        if let Some(event) = evt {
            match event {
                EventType::Response(resp) => {
                    let json = serde_json::to_string(&resp).expect("cant Jsonify");
                    swarm
                        .behaviour_mut()
                        .gossipsub
                        .publish(topic.clone(), json.as_bytes())?;
                }
                // Handle the inputs from stdin.
                EventType::Input(line) => match line.as_str() {
                    "ls p" => list_local_peers(&mut swarm).await,
                    cmd if cmd.starts_with("ls c") => {
                        list_contents(cmd, &mut swarm, topic.clone()).await
                    }
                    cmd if cmd.starts_with("create c") => create_content(cmd).await,
                    cmd if cmd.starts_with("publish c") => handle_publish_content(cmd).await,
                    _ => println!("Invalid command"),
                },
            }
        }
    }
}

// list all local peers discoverd via Mdns
async fn list_local_peers(swarm: &mut Swarm<CustomBehaviour>) {
    println!("Discovered Peers:");
    let nodes = swarm.behaviour().mdns.discovered_nodes();
    let mut unique_peers = HashSet::new();
    for peer in nodes {
        unique_peers.insert(peer);
    }
    unique_peers.iter().for_each(|p| println!("{}", p));
}

// List all the contents
async fn list_contents(cmd: &str, swarm: &mut Swarm<CustomBehaviour>, topic: IdentTopic) {
    let rest = cmd.strip_prefix("ls c ");
    match rest {
        Some("all") => {
            let req = ListRequest {
                mode: ListMode::All,
            };
            let json = serde_json::to_string(&req).expect("can jsonify request");
            swarm
                .behaviour_mut()
                .gossipsub
                .publish(topic, json.as_bytes())
                .unwrap();
        }
        Some(contents_peer_id) => {
            let req = ListRequest {
                mode: ListMode::One(contents_peer_id.to_owned()),
            };
            let json = serde_json::to_string(&req).expect("can jsonify request");
            swarm
                .behaviour_mut()
                .gossipsub
                .publish(topic, json.as_bytes())
                .unwrap();
        }
        None => {
            match read_local_contents().await {
                Ok(v) => {
                    println!("Local Contents ({})", v.len());
                    v.iter().for_each(|r| println!("{:?}", r));
                }
                Err(e) => println!("error fetching local contents: {}", e),
            };
        }
    };
}

async fn create_content(cmd: &str) {
    if let Some(rest) = cmd.strip_prefix("create c") {
        let elements: Vec<&str> = rest.split('|').collect();
        if elements.len() < 2 {
            println!("too few arguments - Format: author|content");
        } else {
            let author = elements.get(0).expect("author is missing");
            let content = elements.get(1).expect("content is missing");
            if let Err(e) = create_new_content(author, content).await {
                println!("error creating content: {}", e);
            };
        }
    }
}

/// Create a new content
async fn create_new_content(author: &str, content: &str) -> Result<()> {
    let mut local_contents = read_local_contents().await?;
    let new_id = match local_contents.iter().max_by_key(|r| r.id) {
        Some(v) => v.id + 1,
        None => 0,
    };
    local_contents.push(Content {
        id: new_id,
        author: author.to_string(),
        content: content.to_string(),
        public: false,
    });
    write_local_contents(&local_contents).await?;

    println!("Created new content: {}: {}", author, content);

    Ok(())
}

/// Read local contents
async fn read_local_contents() -> Result<Contents> {
    let content = fs::read(STORAGE_FILE_PATH).await?;
    let result = serde_json::from_slice(&content)?;
    Ok(result)
}

/// Persist local contents
async fn write_local_contents(contents: &Contents) -> Result<()> {
    let json = serde_json::to_string(&contents)?;
    fs::write(STORAGE_FILE_PATH, &json).await?;
    Ok(())
}

/// Publish a content
async fn handle_publish_content(cmd: &str) {
    if let Some(rest) = cmd.strip_prefix("publish c") {
        match rest.trim().parse::<usize>() {
            Ok(id) => {
                if let Err(e) = publish_content(id).await {
                    println!("error publishing content with id {}, {}", id, e)
                } else {
                    println!("Published content with id: {}", id);
                }
            }
            Err(e) => println!("invalid id: {}, {}", rest.trim(), e),
        };
    }
}

async fn publish_content(id: usize) -> Result<()> {
    let mut local_contents = read_local_contents().await?;
    local_contents
        .iter_mut()
        .filter(|c| c.id == id)
        .for_each(|c| c.public = true);
    write_local_contents(&local_contents).await?;
    Ok(())
}

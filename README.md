A content sharing app build on top of libp2p with Rust.

## P2P Features
### Peer Discovery
A combination of mDNS and libp2p kademlia protocol to to discover peer nodes:
- mDNS protocol is used to discover other nodes on the local network that support libp2p with zero
  configuration
- Kademlia-based DHT that supports the discovery of nodes over the internet. It is used along side
  with boot nodes.

### Peer Routing
Gossipsub, a successor of floodsub is used for peer routing. Gossipsub is an extensible base protocol based on gossip and randomized topic meshes. It is designed with advanced routing properties, amplification factors and optimized properties to transmit messages and gossip for specific application profiles.

As opposed to floodsub that uses an inefficient strategy of network flooding, gossipsub make the network scalable and resilient (Peering agreements, Peer Scoring, Flood Publishing)

## Running the app
In multiple terminals, preferably in different folders, each containing a different non empty `contents.json` file, run with `cargo run` and press enter to start the application.

## Usage
There are several commands supported:

* `ls p` - list all local discovered peers
* `ls c` - list local contents
* `ls c all` - list all public contents from all known peers
* `ls c {peerId}` - list all public contents from the given peer
* `create r Author|Content` - create a new content with the given data, the `|` are important as separators
* `publish r {contentId}` - publish content with the given content ID
 
## Inspirations
Some of the code is inspired from those resources:
- [libp2p tutorial: Build a peer-to-peer app in Rust](https://blog.logrocket.com/libp2p-tutorial-build-a-peer-to-peer-app-in-rust/)
- [Rust libp2p example](https://github.com/libp2p/rust-libp2p/tree/master/examples)
- 
The code is adapted and improved, to support and use more efficient p2p protocols and mechanisms (Peer discovery, Peer routing and content addressing)

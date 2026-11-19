# Feature ideas

## Peer network on port 31415

omp app instances form a secure, decentralized p2p network over LAN, Tailscale and similar, with certificate based, granular access to each other's projects, prompts, sessions and folders.

- **Port** every app instance is a node listening on 31415 (like pi)
- **No central server** nodes talk to each other directly
- **Identity** each node has a public/private keypair. Access is granted by tokens signed with those keys
- **Permission list** each node decides, per peer, what that peer can view, modify and create
- **Resources** projects, prompts, sessions, folders

### Example grants

| Grant | Peer gets | Peer does not get |
| --- | --- | --- |
| One prompt | Prompt A | Everything else |
| One project | Project A | Other projects |
| Prompt + future sessions | Prompt A and every session the node creates from now on | Earlier prompts and projects |

### Design notes

- Grants are capability tokens (Biscuit) bound to the holder's public key, so a copied token is useless without the matching private key
- Permissions split into view, suggest, drive and fork. Fork (copy the session and run it on your own machine and keys) is the default between different people
- Revocation through short lived grants that renew automatically
- The owner node is the single writer of a session. Peers send requests, the owner applies them in order
- "Future sessions only" grants must filter what goes into shared sessions, so earlier context does not leak through project files, memory or digests

### Open questions

- [ ] How is a grant revoked or set to expire
- [ ] How do nodes find each other (manual add, mDNS on LAN, Tailscale peers)
- [ ] Does a grant pass along if a peer re-shares it
- [ ] For a modify grant, whose node runs the agent and holds the API keys

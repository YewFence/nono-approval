# Support Linux and macOS through local platform adapters

The MVP supports both Linux and macOS because webhook ingestion and approval brokering are platform-independent. Platform-specific code is limited to secure runtime-path handling and control peer identity: Linux uses `SO_PEERCRED`, while macOS uses `LOCAL_PEERPID` plus `getpeereid`; either implementation fails closed when identity cannot be verified, following the established approach in nono's `crates/nono/src/supervisor/socket.rs::peer_credentials` without adding a production dependency on the nono crate.

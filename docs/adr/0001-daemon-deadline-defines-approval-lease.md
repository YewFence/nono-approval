# Daemon deadline defines the Approval Lease

The approval daemon uses its monotonic deadline as the sole authority for whether a pending Approval Request may still be decided. A detected webhook disconnect may cancel a request early, but it is only a best-effort cleanup signal because nono can time out its caller while leaving the blocking ApprovalBackend and HTTP connection running; the daemon deadline therefore expires before the generated nono timeout and every decision rechecks it atomically.

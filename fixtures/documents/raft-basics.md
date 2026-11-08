# Raft consensus basics

Raft is a consensus algorithm designed to be easy to understand. It decomposes
consensus into leader election, log replication and safety.

A Raft cluster elects a leader; all writes flow through the leader, which
replicates log entries to followers.

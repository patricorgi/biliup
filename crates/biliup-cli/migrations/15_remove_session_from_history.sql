-- Keep the session ID and its exported clips when the recording is removed from history.
ALTER TABLE stream_sessions ADD COLUMN deleted_at INTEGER;

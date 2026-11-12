CREATE TABLE session_transcripts (
    session_id INTEGER PRIMARY KEY REFERENCES stream_sessions(id) ON DELETE CASCADE,
    cues TEXT NOT NULL
);

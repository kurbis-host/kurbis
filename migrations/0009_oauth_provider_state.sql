TRUNCATE oauth_states;

ALTER TABLE oauth_states
    ADD COLUMN provider text NOT NULL;

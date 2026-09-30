BEGIN;

-- Accounts may own multiple worlds after identities from older accounts merge.
ALTER TABLE servers DROP CONSTRAINT servers_owner_id_key;
ALTER TABLE servers ADD COLUMN is_default boolean NOT NULL DEFAULT false;

CREATE TABLE identities (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider text NOT NULL CHECK (provider IN ('google', 'discord', 'minecraft_java', 'minecraft_bedrock')),
    subject text NOT NULL,
    display_name text NOT NULL,
    email text,
    email_normalized text,
    email_verified boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (provider, subject),
    CHECK (NOT email_verified OR email_normalized IS NOT NULL)
);
CREATE INDEX identities_user_idx ON identities(user_id, created_at, id);
CREATE INDEX identities_verified_email_idx ON identities(email_normalized)
    WHERE email_verified AND email_normalized IS NOT NULL;

INSERT INTO identities (
    id, user_id, provider, subject, display_name, email, email_normalized, email_verified
)
SELECT gen_random_uuid(), id, oauth_provider, oauth_subject, display_name, email,
       CASE WHEN email IS NULL OR regexp_replace(email, '^[[:space:]]+|[[:space:]]+$', '', 'g') = ''
            THEN NULL
            ELSE lower(regexp_replace(email, '^[[:space:]]+|[[:space:]]+$', '', 'g'))
       END,
       false
FROM users;

ALTER TABLE users DROP COLUMN oauth_provider;
ALTER TABLE users DROP COLUMN oauth_subject;
ALTER TABLE users DROP COLUMN email;

-- Expired legacy sessions were already invalid under the previous contract.
DELETE FROM sessions WHERE expires_at <= now();
ALTER TABLE sessions DROP COLUMN expires_at;
ALTER TABLE sessions ADD COLUMN id uuid;
UPDATE sessions SET id = gen_random_uuid();
ALTER TABLE sessions ALTER COLUMN id SET NOT NULL;
ALTER TABLE sessions ADD CONSTRAINT sessions_id_key UNIQUE (id);
ALTER TABLE sessions ADD COLUMN last_seen_at timestamptz NOT NULL DEFAULT now();

ALTER TABLE oauth_states ADD COLUMN intent text NOT NULL DEFAULT 'login'
    CHECK (intent IN ('login', 'link'));
ALTER TABLE oauth_states ALTER COLUMN intent DROP DEFAULT;
ALTER TABLE oauth_states ADD COLUMN user_id uuid REFERENCES users(id) ON DELETE CASCADE;

CREATE TABLE minecraft_login_tokens (
    token_hash bytea PRIMARY KEY,
    provider text NOT NULL CHECK (provider IN ('minecraft_java', 'minecraft_bedrock')),
    subject text NOT NULL,
    display_name text NOT NULL,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX minecraft_login_tokens_expiry_idx ON minecraft_login_tokens(expires_at);

-- Preserve one default world per account; migrated accounts choose their oldest.
WITH ranked AS (
    SELECT id,
           row_number() OVER (PARTITION BY owner_id ORDER BY created_at, id) AS position
    FROM servers
)
UPDATE servers s SET is_default = (ranked.position = 1)
FROM ranked WHERE ranked.id = s.id;

CREATE UNIQUE INDEX servers_one_default_per_owner_idx
    ON servers(owner_id) WHERE is_default;

CREATE FUNCTION choose_default_server_before_write() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.owner_id = OLD.owner_id THEN
        RETURN NEW;
    END IF;

    PERFORM id FROM users WHERE id = NEW.owner_id FOR UPDATE;
    IF NOT NEW.is_default AND NOT EXISTS (
        SELECT 1 FROM servers WHERE owner_id = NEW.owner_id AND is_default
    ) THEN
        NEW.is_default := true;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER servers_choose_default_before_write
BEFORE INSERT OR UPDATE OF owner_id ON servers
FOR EACH ROW EXECUTE FUNCTION choose_default_server_before_write();

CREATE FUNCTION replace_deleted_default_server() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.is_default THEN
        UPDATE servers
        SET is_default = true
        WHERE id = (
            SELECT id FROM servers
            WHERE owner_id = OLD.owner_id
            ORDER BY created_at, id
            LIMIT 1
        );
    END IF;
    RETURN OLD;
END;
$$;

CREATE TRIGGER servers_replace_deleted_default
AFTER DELETE ON servers
FOR EACH ROW EXECUTE FUNCTION replace_deleted_default_server();

COMMIT;

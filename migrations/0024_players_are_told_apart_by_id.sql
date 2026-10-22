-- A whitelist or operator entry is a player, and players are told apart by
-- their UUID (the primary key and the unique index on it), not by name.
--
-- The unique indexes on lower(name) made two different players collide when
-- their names differ only in case or are shared between editions: Java
-- "Flam3rBoy" is on a whitelist, Bedrock "Flam3rboy" is another account, and
-- adding or letting the second in failed with "already whitelisted" while the
-- server kept turning them away. Resolving a name to a player already refuses
-- an ambiguous one (ambiguous_player_name), so nothing relied on the index.
DROP INDEX "public"."whitelist_entries_server_name_idx";
DROP INDEX "public"."operator_entries_server_name_idx";

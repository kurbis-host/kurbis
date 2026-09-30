-- All inputs to route resolution must invalidate gateway caches. The payload
-- is only a wake-up signal; gateways still resolve authoritative Postgres data.
CREATE TRIGGER users_route_change
AFTER UPDATE OF account_status ON users
FOR EACH ROW EXECUTE FUNCTION notify_server_route_change();

CREATE TRIGGER workers_route_change
AFTER UPDATE OF address, heartbeat_at ON workers
FOR EACH ROW EXECUTE FUNCTION notify_server_route_change();

DROP TRIGGER servers_route_change ON servers;
CREATE TRIGGER servers_route_change
AFTER INSERT OR DELETE OR UPDATE OF observed_state, desired_state, subdomain ON servers
FOR EACH ROW EXECUTE FUNCTION notify_server_route_change();

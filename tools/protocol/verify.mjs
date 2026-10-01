import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { parseArgs } from 'node:util';
import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';

const require = createRequire(import.meta.url);
require('./raknet11.cjs');
// Import the client directly: the convenience createClient module initializes
// a native RakNet backend even with skipPing. These tests use actual JS RakNet.
const { Client: BedrockClient } = require('bedrock-protocol/src/client');
const minecraft = require('minecraft-protocol');
const { values } = parseArgs({ options: {
  edition: { type: 'string' }, host: { type: 'string', default: '127.0.0.1' },
  port: { type: 'string' }, address: { type: 'string' },
  username: { type: 'string', default: 'RuntimePlayer' },
  'transfer-port': { type: 'string' }, 'transfer-host': { type: 'string', default: '127.0.0.1' },
  'expect-kick': { type: 'string' }, 'hold-seconds': { type: 'string', default: '2' },
  'expect-close': { type: 'boolean' },
  timeout: { type: 'string', default: '180' }, output: { type: 'string' },
  command: { type: 'string', multiple: true }, 'expect-item': { type: 'string', multiple: true },
  'expect-message': { type: 'string', multiple: true },
  direct: { type: 'boolean' },
} });
assert(['java', 'bedrock'].includes(values.edition), '--edition must be java or bedrock');
assert(values.address && values.port && values.output, '--address, --port and --output required');
const output = path.resolve(values.output);
const evidence = { edition: values.edition, address: values.address, events: [] };
const connected = new Set();
const timers = new Set();
function record(event, data = {}) {
  const entry = { event, at: new Date().toISOString(), ...data };
  evidence.events.push(entry);
  console.log(JSON.stringify(entry));
}
function timer(callback, milliseconds) {
  const id = setTimeout(callback, milliseconds);
  timers.add(id);
  return id;
}
function close(client) {
  connected.delete(client);
  if (values.edition === 'java') client.end();
  else {
    // disconnect() returns early before RakNet connects; close() must still
    // cancel its pending transport and connection timeout in that state.
    try { client.write('disconnect', { reason: 'user_leave_game_attempted', hide_disconnect_reason: true }); }
    finally { client.close(); }
  }
}
function connect(host, port, address, transferred = false) {
  if (values.edition === 'java') {
    const client = minecraft.createClient({ host, port, fakeHost: address,
      username: values.username, auth: 'offline', version: '26.1',
      hideErrors: false, checkTimeoutInterval: 30000, clientSettings: { viewDistance: 2 } });
    if (transferred) {
      const write = client.write.bind(client);
      client.write = (name, data) => write(name, name === 'set_protocol' ? { ...data, nextState: 3 } : data);
    }
    return client;
  }
  return new BedrockClient({ host, port, username: values.username,
    version: '1.26.40', offline: true, raknetBackend: 'jsp-raknet',
    useRaknetWorkers: false, delayedInit: true, autoInitPlayer: true,
    viewDistance: 2, conLog: null, connectTimeout: 30000,
    skinData: { ServerAddress: address } });
}

try {
  await new Promise((resolve, reject) => {
    let finished = false;
    let transferred = false;
    const fail = error => { if (!finished) { finished = true; reject(error); } };
    const success = () => { if (!finished) { finished = true; resolve(); } };
    timer(() => fail(new Error('Protocol verification timed out')), Number(values.timeout) * 1000);
    function observe(client, backend, direct = false) {
      connected.add(client);
      let joined = false;
      const inventory = new Map();
      const messages = [];
      function assertInventory() {
        const wanted = values['expect-item'] ?? [];
        const data = require('minecraft-data')('26.1');
        for (const expected of wanted) {
          const [name, count] = expected.split(':');
          const item = data.itemsByName[name];
          assert(item, `Unknown expected item ${name}`);
          const actual = [...inventory.values()].filter(slot => slot?.itemId === item.id)
            .reduce((sum, slot) => sum + slot.itemCount, 0);
          assert.equal(actual, Number(count), `Inventory ${name} after login/commands`);
          record('inventory_verified', { name, count: actual });
        }
      }
      if (values.edition === 'java') {
        client.on('system_chat', packet => { messages.push(JSON.stringify(packet)); record('server_message', { packet }); });
        client.on('window_items', packet => {
          if (packet.windowId === 0) packet.items.forEach((slot, index) => inventory.set(index, slot));
        });
        client.on('set_slot', packet => {
          if (packet.windowId === 0 || packet.windowId === -2) inventory.set(packet.slot, packet.item);
        });
        client.on('position', packet => client.write('teleport_confirm', { teleportId: packet.teleportId }));
        client.once('position', packet => {
          record('world_spawned', { position: packet });
          if (!backend && values['transfer-port']) return;
          for (const command of values.command ?? []) {
            client.write('chat_command', { command });
            record('command_sent', { command });
          }
        });
      }
      const joinedEvent = values.edition === 'java' ? 'login' : 'spawn';
      client.once(joinedEvent, () => {
        joined = true;
        record(backend || direct ? 'backend_joined' : 'waiting_joined', { username: values.username });
      });
      client.once(values.edition === 'java' ? 'position' : 'spawn', () => {
        if (!values['expect-kick'] && !values['expect-close'] && (backend || !values['transfer-port'])) {
          timer(() => {
            try {
              assertInventory();
              for (const expected of values['expect-message'] ?? []) assert(messages.some(message => message.includes(expected)), `Missing server acknowledgment: ${expected}`);
              success();
            } catch (error) { fail(error); }
          }, Number(values['hold-seconds']) * 1000);
        }
      });
      client.on('error', fail);
      client.on(values.edition === 'java' ? 'end' : 'close', reason => {
        if (finished || !connected.has(client)) return;
        if (values['expect-close'] && joined) {
          record('connection_closed', { reason: String(reason) });
          success();
          return;
        }
        fail(new Error(`Connection closed before completion: ${String(reason)}`));
      });
      const kicked = packet => {
        const text = JSON.stringify(packet);
        record('kicked', { packet });
        if (values['expect-kick'] && text.includes(values['expect-kick'])) success();
        else if (values['expect-close'] && joined) success();
        else fail(new Error(`Unexpected kick: ${text}`));
      };
      client.on(values.edition === 'java' ? 'disconnect' : 'kick', kicked);
      client.on('transfer', packet => {
        if (backend || transferred || !values['transfer-port']) {
          fail(new Error('Unexpected or repeated native transfer'));
          return;
        }
        try {
          const host = packet.host ?? packet.server_address;
          assert.equal(packet.port, Number(values['transfer-port']));
          assert.equal(host, values['transfer-host']);
          assert(joined, 'Transfer must follow a completed waiting login');
          transferred = true;
          record('native_transfer', { host, port: packet.port });
          close(client);
          observe(connect(host, packet.port, `${host}:${packet.port}`, true), true);
        } catch (error) { fail(error); }
      });
      if (values.edition === 'bedrock') {
        client.on('packet', packet => {
          const name = packet.data.name;
          (evidence.packetCounts ??= {})[name] = (evidence.packetCounts[name] ?? 0) + 1;
        });
        for (const name of ['join', 'resource_packs_info', 'resource_pack_stack', 'start_game', 'chunk_radius_update']) {
          client.on(name, () => record(name));
        }
        client.once('start_game', () => client.queue('request_chunk_radius', { chunk_radius: 2 }));
        // Use the pinned protocol's disconnect fields also for library timeouts.
        client.disconnect = () => close(client);
        client.on('connect_allowed', () => {
          // Transport keeps its physical destination; the signed login carries
          // the logical hostname, as a client joining wildcard DNS would.
          client.options.host = backend ? values['transfer-host'] : values.address;
          const parse = client.deserializer.parsePacketBuffer.bind(client.deserializer);
          client.deserializer.parsePacketBuffer = packet => {
            try { return parse(packet); }
            catch (error) { evidence.failedPacketHex ??= packet.toString('hex'); (evidence.failedPackets ??= []).push({ hex: packet.toString('hex'), error: String(error) }); throw error; }
          };
          // The JS adapter starts plainConnect without observing its promise.
          // Surface real connection failures, and handle cancellation after our
          // own deadline closes the socket while its retry is still pending.
          const connect = client.connection.connect.bind(client.connection);
          client.connection.connect = async () => {
            try { await connect(); }
            catch (error) { if (connected.has(client)) fail(error); }
          };
          client.connect();
        });
        client.on('network_stack_latency', packet => {
          if (packet.needs_response) client.queue('network_stack_latency', { timestamp: packet.timestamp, needs_response: false });
        });
        function packsCompleted() {
          client.write('resource_pack_client_response', { response_status: 'completed',
            response_status_name: 'resourcepackstackfinished', resourcepackids: [] });
        }
        client.once('resource_packs_info', () => {
          packsCompleted();
          client.queue('client_cache_status', { enabled: false });
          timer(() => client.queue('request_chunk_radius', { chunk_radius: 2 }), 500);
        });
        client.once('resource_pack_stack', packsCompleted);
        client.init();
      }
    }
    observe(connect(values.host, Number(values.port), values.address), false, values.direct === true);
  });
  evidence.passed = true;
} catch (error) {
  evidence.passed = false;
  evidence.error = String(error.stack ?? error);
  process.exitCode = 1;
} finally {
  for (const id of timers) clearTimeout(id);
  for (const client of connected) close(client);
  await mkdir(path.dirname(output), { recursive: true });
  await writeFile(output, JSON.stringify(evidence, null, 2) + '\n');
}

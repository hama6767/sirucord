// Manual, read-only diagnostic. Never log tokens, IDs, URLs, or response bodies.
import { pathToFileURL } from 'node:url';

export function targetsFrom(input) {
  if (!Array.isArray(input) || !input.length) throw new Error('Invalid targets');
  const targets = new Map();
  for (const item of input) {
    if (!/^\d+$/.test(item.guild_id) || !Array.isArray(item.voice_channel_ids)
        || !item.voice_channel_ids.length || item.voice_channel_ids.some(id => !/^\d+$/.test(id))) {
      throw new Error('Invalid target');
    }
    const channels = targets.get(item.guild_id) ?? new Set();
    item.voice_channel_ids.forEach(id => channels.add(id));
    targets.set(item.guild_id, channels);
  }
  return targets;
}

export function streamKeys(guild, channels) {
  if (!Array.isArray(guild.voice_states) || !Array.isArray(guild.members)) {
    throw new Error('Incomplete snapshot');
  }
  const members = new Map(guild.members.map(member => [member.user.id, member.user]));
  return guild.voice_states.filter(voice => {
    if (!channels.has(voice.channel_id) || voice.self_stream !== true) return false;
    const user = members.get(voice.user_id) ?? voice.member?.user;
    if (!user) throw new Error('Missing member');
    return !user.bot;
  }).map(voice => {
    if (![guild.id, voice.channel_id, voice.user_id].every(id => /^\d+$/.test(id))) {
      throw new Error('Invalid stream key');
    }
    return `guild:${guild.id}:${voice.channel_id}:${voice.user_id}`;
  });
}

export function previewUrl(value, key) {
  const url = new URL(value);
  if (url.origin !== 'https://cdn.discordapp.com' || url.username || url.password
      || !decodeURIComponent(url.pathname).startsWith(`/streams/${key}/`)) {
    throw new Error('Unexpected preview URL');
  }
  return url;
}

async function api(path, token) {
  const response = await fetch(`https://discord.com/api/v10${path}`, {
    headers: { Authorization: `Bot ${token}`, 'User-Agent': 'Sirucord preview diagnostic' },
    redirect: 'error', signal: AbortSignal.timeout(15000),
  });
  const data = await response.json().catch(() => ({}));
  return { status: response.status, data };
}

async function discover(targets, token, gateway) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(`${gateway}?v=10&encoding=json`);
    const remaining = new Set(targets.keys());
    const keys = [];
    let heartbeat, sequence = null, done = false;
    const finish = (error) => {
      if (done) return;
      done = true;
      clearTimeout(timeout);
      clearInterval(heartbeat);
      socket.close();
      if (error) reject(error); else resolve(keys);
    };
    const timeout = setTimeout(() => finish(new Error('Snapshot timeout')), 45000);
    socket.onerror = () => finish(new Error('Gateway error'));
    socket.onclose = () => finish(new Error('Gateway closed before snapshot'));
    socket.onmessage = event => {
      if (done) return;
      try {
        const message = JSON.parse(event.data);
        if (message.s != null) sequence = message.s;
        if (message.op === 10) {
          const beat = () => socket.send(JSON.stringify({ op: 1, d: sequence }));
          heartbeat = setInterval(beat, message.d.heartbeat_interval);
          beat();
          socket.send(JSON.stringify({ op: 2, d: {
            token, intents: 129,
            properties: { os: 'linux', browser: 'sirucord-preview', device: 'sirucord-preview' },
          } }));
        } else if (message.op === 1) {
          socket.send(JSON.stringify({ op: 1, d: sequence }));
        } else if (message.op === 7 || message.op === 9) {
          finish(new Error('Gateway session unavailable'));
        } else if (message.t === 'GUILD_CREATE' && remaining.has(message.d.id)) {
          if (message.d.unavailable) throw new Error('Guild unavailable');
          keys.push(...streamKeys(message.d, targets.get(message.d.id)));
          remaining.delete(message.d.id);
          if (!remaining.size) finish();
        }
      } catch { finish(new Error('Invalid Gateway snapshot')); }
    };
  });
}

async function main() {
  const targets = targetsFrom(JSON.parse(process.env.PREVIEW_TARGETS));
  const token = process.env.DISCORD_BOT_TOKEN?.trim();
  if (!token) throw new Error('Missing token');
  const gateway = await api('/gateway/bot', token);
  if (gateway.status !== 200) {
    console.log(`Bot authentication: HTTP ${gateway.status}.`);
    return;
  }
  if (!(gateway.data.session_start_limit?.remaining > 0)) throw new Error('Session budget exhausted');
  if (gateway.data.url !== 'wss://gateway.discord.gg') throw new Error('Unexpected Gateway URL');
  console.log('Bot authentication: OK.');
  const keys = await discover(targets, token, gateway.data.url);
  console.log(`Active human streams in configured channels: ${keys.length}.`);
  if (!keys.length) {
    console.log('INCONCLUSIVE: no active stream to test. Run manually while someone is streaming.');
    return;
  }
  // One genuine stream is enough to test bot authentication. No retries or auth fallbacks.
  const key = keys[0];
  const result = await api(`/streams/${encodeURIComponent(key)}/preview`, token);
  const code = Number.isSafeInteger(result.data.code) ? result.data.code : 'none';
  console.log(`Stream preview: HTTP ${result.status}; Discord code ${code}.`);
  if (code === 20001) {
    console.log('REJECTED: Discord says bots cannot use this endpoint.');
    return;
  }
  if (result.status !== 200 || typeof result.data.url !== 'string') {
    console.log('No preview URL obtained. This result alone may not establish bot support.');
    return;
  }
  const url = previewUrl(result.data.url, key);
  // CDN request is unauthenticated, bounded, and does not persist or publish the image.
  const response = await fetch(url, { redirect: 'error', signal: AbortSignal.timeout(15000) });
  console.log(`Preview image download: HTTP ${response.status}.`);
  if (!response.ok) return;
  const chunks = [];
  let size = 0;
  for await (const chunk of response.body) {
    size += chunk.length;
    if (size > 5 * 1024 * 1024) throw new Error('Preview too large');
    chunks.push(chunk);
  }
  const image = Buffer.concat(chunks);
  const valid = image.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]))
    || image.subarray(0, 3).equals(Buffer.from([255, 216, 255]))
    || (image.toString('ascii', 0, 4) === 'RIFF' && image.toString('ascii', 8, 12) === 'WEBP');
  console.log(valid ? `SUCCESS: preview image obtained (${size} bytes); not saved or posted.`
    : 'INCONCLUSIVE: response did not have a supported image signature.');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(() => {
    console.error('Preview diagnostic failed: network, configuration, or response error (details redacted).');
    process.exitCode = 1;
  });
}

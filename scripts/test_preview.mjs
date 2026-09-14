import assert from 'node:assert/strict';
import { test } from 'node:test';
import { targetsFrom, streamKeys, previewUrl } from './check_preview.mjs';

test('only configured live human streams are probed', () => {
  const guild = { id: '1', members: [
    { user: { id: '2' } }, { user: { id: '3', bot: true } },
  ], voice_states: [
    { user_id: '2', channel_id: '4', self_stream: true },
    { user_id: '3', channel_id: '4', self_stream: true },
    { user_id: '2', channel_id: '5', self_stream: true },
    { user_id: '2', channel_id: '4', self_stream: false },
  ] };
  assert.deepEqual(streamKeys(guild, new Set(['4'])), ['guild:1:4:2']);
  guild.members = [];
  assert.throws(() => streamKeys(guild, new Set(['4'])));
});

test('configuration requires explicit numeric guild and channel IDs', () => {
  assert.throws(() => targetsFrom([]));
  assert.throws(() => targetsFrom([{ guild_id: '1', voice_channel_ids: ['../2'] }]));
  assert.deepEqual([...targetsFrom([{ guild_id: '1', voice_channel_ids: ['2'] }]).get('1')], ['2']);
});

test('only the tested stream on the exact Discord CDN origin is downloaded', () => {
  const key = 'guild:1:2:3';
  assert.equal(previewUrl(`https://cdn.discordapp.com/streams/${key}/hash`, key).hostname,
    'cdn.discordapp.com');
  for (const url of [
    `https://cdn.discordapp.com.evil.example/streams/${key}/hash`,
    `https://evil.example/streams/${key}/hash`,
    `http://cdn.discordapp.com/streams/${key}/hash`,
    'https://cdn.discordapp.com/streams/guild:1:2:4/hash',
    `https://secret@cdn.discordapp.com/streams/${key}/hash`,
  ]) assert.throws(() => previewUrl(url, key));
});

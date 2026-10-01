// Real producer evidence for the hook/supervisor emote matrix. No live mail root.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
export function porchSnapshots(binary, { ordinary = false, cursor = 'healthy' } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'porch-post-'));
  const mail = path.join(root, 'mail');
  const env = { PATH: process.env.PATH, HOME: root, POST_MAIL_ROOT: mail };
  function post(args, id) {
    const r = spawnSync(binary, args, { env: { ...env, ...(id ? { POST_PARTICIPANT: id } : {}) }, cwd: root, encoding: 'utf8', timeout: 15000 });
    if (r.status !== 0 || r.error) throw new Error(`post ${args.join(' ')}: ${r.error ?? r.stderr}`);
    return r.stdout;
  }
  try {
    const ids = [];
    for (const name of ['alpha', 'beta']) {
      const cwd = path.join(root, name); fs.mkdirSync(cwd);
      post(['rooms', 'add', name, cwd]);
      const v = JSON.parse(post(['participant', 'bind', '--new', '--workspace', name, '--json']));
      ids.push(v.id ?? v.participant.id);
    }
    for (const id of ids) post(['chat', 'ops', '--join', '--json'], id);
    for (const id of ids) post(['chat', 'ops', '--json'], id);
    const dir = path.join(mail, 'channels', 'ops', 'messages');
    for (const name of fs.readdirSync(dir)) fs.rmSync(path.join(dir, name));
    fs.writeFileSync(path.join(mail, 'channels', 'ops', 'members.json'), JSON.stringify({ alpha: '2026-09-30 10:00:00 +0000', beta: '2026-09-30 10:00:00 +0000' }));
    const head = (id) => ({ id, from: 'alpha', from_participant: ids[0], channel: 'ops', sent: '2099-09-30 10:00:00 +0000', subject: '' });
    if (ordinary) {
      for (const [id, body] of [['20990930-100000-000001-aaaaaa', 'ordinary'], ['20990930-100000-000003-aaaaaa', '@beta ordinary mention']]) {
        fs.writeFileSync(path.join(dir, `${id}.msg`), `${JSON.stringify(head(id))}\n---\n${body}`);
      }
    }
    const cursors = path.join(mail, 'participants', ids[1], 'cursors.json');
    if (cursor === 'missing') fs.rmSync(cursors, { force: true });
    if (cursor === 'corrupt') fs.writeFileSync(cursors, 'broken');
    const snapshot = () => ({
      bound: post(['watch', '--snapshot', '--limit', '0', '--json'], ids[1]),
      room: post(['watch', '--snapshot', '--room', 'beta', '--limit', '0', '--json']),
    });
    const before = snapshot();
    const raw = fs.readFileSync(path.join(REPO, 'tests/fixtures/porch-contract/emotes/records/playable/body-ignored.emote'), 'utf8');
    const emote = JSON.parse(raw.split('\n---\n')[0]);
    Object.assign(emote, head('20990930-100000-000002-aaaaaa'), { event: 'emote', mentions: ['beta'] });
    emote.emote.at = ids[1]; emote.emote.planted = `@beta @${ids[1]}`;
    fs.writeFileSync(path.join(dir, `${emote.id}.emote`), `${JSON.stringify(emote)}\n---\n@beta`);
    fs.writeFileSync(path.join(dir, '20990930-100000-000004-aaaaaa.emote'), 'corrupt @beta');
    emote.id = '20990930-100000-000005-aaaaaa';
    fs.writeFileSync(path.join(dir, `${emote.id}.msg`), `${JSON.stringify(emote)}\n---\n@beta`);
    const after = snapshot();
    return { id: ids[1], before, after };
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
}

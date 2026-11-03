#!/usr/bin/env node
import { basename } from 'node:path';

const mode = basename(process.argv[2]);
if (mode === 'timeout' || mode === 'stubborn') {
  if (mode === 'stubborn') process.on('SIGTERM', () => {});
  setInterval(() => {}, 1000);
} else if (mode === 'crash') {
  process.exit(3);
} else if (mode === 'malformed') {
  process.stdout.write('not json');
} else if (mode === 'invalid-utf8') {
  process.stdout.write(Buffer.concat([Buffer.from('{"text":["'), Buffer.from([0xff]), Buffer.from('"],"pos":0,"endOfWord":false}')]));
} else if (mode === 'overflow') {
  process.stdout.write('x'.repeat(300000));
} else {
  let input = '';
  process.stdin.setEncoding('utf8');
  process.stdin.on('data', chunk => { input += chunk; });
  process.stdin.on('end', () => {
    const request = JSON.parse(input);
    const response = { text: [request.q], pos: 0, endOfWord: false };
    if (process.argv.length !== 3) process.exit(4); // query must never be argv
    if (mode === 'bad-pos') response.pos = -999;
    if (mode === 'split-surrogate') response.pos = -1;
    if (mode === 'slow') setTimeout(() => console.log(JSON.stringify(response)), 100);
    else console.log(JSON.stringify(response));
  });
}

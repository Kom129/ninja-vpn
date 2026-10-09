// «Окна в страны»: рисует картинки мест через Codex (в стиле образца ref-style-a.png).
//   node scripts/gen-places.mjs            — все, которых ещё нет в docs/design/places
//   node scripts/gen-places.mjs nl de      — только эти (перерисовать)
// Codex сохраняет картинку в свою папку и печатает путь — забираем её как docs/design/places/<код>.png.
// Потом: node scripts/build-places.mjs — уменьшить для окна программы.

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const DIR = path.resolve(import.meta.dirname, '..', '..', 'docs', 'design', 'places');
const CODEX = path.join(path.dirname(process.execPath), 'node_modules', '@openai', 'codex', 'bin', 'codex.js');
const PARALLEL = 3;

/** Код страны → что изображено. Одно узнаваемое место, понятное в кружке 100 px. */
const PLACES = {
  nl: 'Amsterdam canal houses with a brick bridge, bicycles and a windmill in the distance',
  de: 'Neuschwanstein castle on a green hill in front of the Bavarian Alps',
  us: 'the Statue of Liberty in front of the Manhattan skyline across the water',
  fr: 'the Eiffel Tower above Paris rooftops and the river Seine',
  fi: 'a calm Finnish lake with pine forest and a small red wooden cottage on the shore',
  pl: 'Krakow old town market square with the Cloth Hall and St. Mary\'s Basilica towers',
  tr: 'colorful hot-air balloons floating over the fairy chimneys of Cappadocia at sunrise',
  ae: 'the Dubai skyline with the Burj Khalifa above calm water and palm trees',
  jp: 'Mount Fuji with a red five-story pagoda and pink cherry blossoms',
  kz: 'the turquoise Big Almaty Lake surrounded by snowy Tian Shan mountains and fir trees',
  ru: 'the Moscow Kremlin wall and colorful St. Basil\'s Cathedral domes on Red Square',
  al: 'a turquoise bay of the Albanian Riviera with white pebbles, small islands and green hills',
  ua: 'golden church domes of Kyiv on a green hill above the wide Dnipro river',
  au: 'the Sydney Opera House and Harbour Bridge on the blue harbour',
  hk: 'the Hong Kong Victoria Harbour skyline with a red-sailed junk boat',
  in: 'the Taj Mahal with its long reflecting pool and gardens',
  ar: 'the jagged Mount Fitz Roy peaks in Patagonia above a turquoise glacial lake',
  br: 'Rio de Janeiro: the Christ the Redeemer statue on its mountain above Sugarloaf and the bay',
  ca: 'Moraine Lake with turquoise water, a wooden canoe and the Rocky Mountains',
  sg: 'Singapore Marina Bay Sands and the glowing Supertrees of Gardens by the Bay',
  md: 'rolling Moldovan vineyards and the Orheiul Vechi cave monastery on a cliff above a river bend',
  ng: 'the huge Zuma Rock rising above green Nigerian savanna and a few baobab trees',
  gb: 'Big Ben and the Houses of Parliament by the river Thames with a red double-decker bus',
  se: 'Stockholm Gamla Stan colorful waterfront houses with a small sailboat',
  ch: 'the Matterhorn above a green alpine meadow with a wooden chalet',
  at: 'Hallstatt lakeside village with its church spire and mountains reflected in the lake',
  lv: 'Riga old town with the tall church spire and colorful gabled houses',
  lt: 'Vilnius old town with the white Cathedral bell tower and red roofs',
  ee: 'Tallinn old town with medieval towers, red roofs and the city wall',
  es: 'the Sagrada Familia basilica in Barcelona with palm trees',
  it: 'the Colosseum in Rome under a bright sky with umbrella pines',
  cz: 'Prague Charles Bridge with its statues and the castle on the hill behind',
  world: 'a small friendly planet Earth floating among soft clouds with tiny famous landmarks around it (a tower, a pyramid, a bridge), like a miniature globe diorama',
};

const prompt = (scene, file) =>
  'Generate ONE image with your image generation tool. ' +
  'Copy the visual style of the attached reference EXACTLY: soft 3D clay miniature diorama, gentle sunny lighting, rounded simplified shapes, bright clean colors, pale blue sky with soft clouds. ' +
  `Scene: ${scene}. ` +
  'Square 1:1, full-bleed (no circle, no frame, no border). The app crops it to a circle, so keep the landmark in the central 70% of the picture, sky in the top third. ' +
  'It must read clearly when shrunk to 100 px: one landmark, bold simple composition, no small clutter. No text, no letters, no flags, no people close-up. ' +
  `Save the resulting PNG into the current working directory as ${file}. Then print the absolute path.`;

function generate(code) {
  return new Promise((resolve) => {
    const file = `${code}.png`;
    const args = [CODEX, 'exec', '--skip-git-repo-check', '--sandbox', 'workspace-write', '-m', 'gpt-6-astra', '-c', 'model_reasoning_effort=high', '--image', 'ref-style-a.png', '--', prompt(PLACES[code], file)];
    const child = spawn(process.execPath, args, { cwd: DIR, stdio: ['ignore', 'pipe', 'pipe'] });
    let out = '';
    child.stdout.on('data', (d) => (out += d));
    child.stderr.on('data', (d) => (out += d));
    child.on('close', () => {
      const target = path.join(DIR, file);
      if (!fs.existsSync(target)) {
        // Codex часто кладёт картинку в свою папку и печатает путь — забираем оттуда.
        const saved = [...out.matchAll(/[A-Z]:\\[^\s`'"]+?\.png/gi)].map((m) => m[0]).reverse().find((p) => fs.existsSync(p));
        if (saved) fs.copyFileSync(saved, target);
      }
      const ok = fs.existsSync(target);
      console.log(`${ok ? 'готово' : 'НЕ ВЫШЛО'}: ${code}${ok ? '' : '\n' + out.split('\n').slice(-6).join('\n')}`);
      resolve(ok);
    });
  });
}

const wanted = process.argv.slice(2).length ? process.argv.slice(2) : Object.keys(PLACES).filter((c) => !fs.existsSync(path.join(DIR, `${c}.png`)));
console.log(`Рисую ${wanted.length}: ${wanted.join(', ')}`);
const queue = [...wanted];
const failed = [];
await Promise.all(
  Array.from({ length: PARALLEL }, async () => {
    while (queue.length) {
      const code = queue.shift();
      if (!(await generate(code))) failed.push(code);
    }
  }),
);
console.log(failed.length ? `Не вышли: ${failed.join(', ')} — запусти ещё раз с этими кодами` : 'Все готовы');

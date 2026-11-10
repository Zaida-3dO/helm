// Dev fake for the cards an agent puts in the chat: the picture galleries (app_show_images) and
// option boards (app_offer_options) of the scripted turn, so they can be seen and tried in a plain
// browser. The pictures are small generated SVGs; a board remembers its one answer like the host.

import type { FakeOpHandler } from './fakeHost.ts';

interface FakeImage {
  name: string;
  caption: string | null;
  bytes: string;
  width: number;
  height: number;
}

interface FakeBoard {
  chatId: string;
  question: string;
  multiSelect: boolean;
  allowOther: boolean;
  createdAt: number;
  options: { label: string; detail: string | null; image: FakeImage | null }[];
  answer: { labels: string[]; other: string | null; answeredAt: number } | null;
}

const galleries = new Map<string, { chatId: string; title: string | null; createdAt: number; images: FakeImage[] }>();
const boards = new Map<string, FakeBoard>();
let serial = 0;

const COLOURS = ['#c4553f', '#3f7fc4', '#4aa06a', '#c49a3f', '#8a4fbb', '#3fa3a3'];

function svg(label: string, colour: string, width = 640, height = 420): FakeImage {
  const text = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}"><rect width="100%" height="100%" fill="${colour}"/><text x="50%" y="50%" fill="#fff" font-family="sans-serif" font-size="44" text-anchor="middle" dominant-baseline="middle">${label}</text></svg>`;
  return { name: `${label.toLowerCase().replace(/\s+/g, '-')}.svg`, caption: null, bytes: btoa(text), width, height };
}

/** A gallery the scripted turn shows: `count` generated pictures with captions. */
export function fakeGallery(chatId: string, count = 4, title: string | null = 'Banner options'): { galleryId: string; count: number } {
  const galleryId = `fake-gallery-${++serial}`;
  const images = Array.from({ length: count }, (_, i) => ({ ...svg(`Option ${i + 1}`, COLOURS[i % COLOURS.length]!, i % 2 ? 480 : 640, 420), caption: `Option ${i + 1}: ${['warm', 'cool', 'green', 'gold', 'plum', 'teal'][i % 6]} tones` }));
  galleries.set(galleryId, { chatId, title, createdAt: Date.now(), images });
  return { galleryId, count };
}

/** An option board the scripted turn offers. */
export function fakeBoard(chatId: string, question: string, labels: string[], opts: { multiSelect?: boolean; withImages?: boolean } = {}): string {
  const boardId = `fake-board-${++serial}`;
  boards.set(boardId, {
    chatId,
    question,
    multiSelect: opts.multiSelect === true,
    allowOther: true,
    createdAt: Date.now(),
    options: labels.map((label, i) => ({ label, detail: i % 2 === 0 ? `A ${label.toLowerCase()} look for the bar` : null, image: opts.withImages === false ? null : svg(label, COLOURS[i % COLOURS.length]!, 320, 240) })),
    answer: null,
  });
  return boardId;
}

function missing(): never {
  throw { code: 'not_found', message: 'That is gone.' };
}

function boardState(id: string) {
  const b = boards.get(id) ?? missing();
  return {
    boardId: id,
    question: b.question,
    multiSelect: b.multiSelect,
    allowOther: b.allowOther,
    createdAt: b.createdAt,
    options: b.options.map((o, index) => ({ index, label: o.label, detail: o.detail, hasImage: o.image !== null })),
    answer: b.answer,
  };
}

function pictureOf(id: string, index: number): FakeImage {
  const img = galleries.get(id)?.images[index] ?? boards.get(id)?.options[index]?.image;
  return img ?? missing();
}

export function cardsFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const handler: FakeOpHandler = (op, args) => {
    const id = String(args.galleryId ?? args.boardId ?? '');
    switch (op) {
      case 'shown_gallery_get': {
        const g = galleries.get(id) ?? missing();
        return {
          galleryId: id,
          title: g.title,
          createdAt: g.createdAt,
          images: g.images.map((im, index) => ({ index, name: im.name, caption: im.caption, mime: 'image/svg+xml', width: im.width, height: im.height })),
        };
      }
      case 'shown_image_get':
      case 'shown_image_thumb':
        return { mime: 'image/svg+xml', bytes: pictureOf(id, Number(args.index)).bytes };
      case 'option_board_state':
        return boardState(id);
      case 'option_board_answer': {
        const b = boards.get(id) ?? missing();
        if (b.answer) throw { code: 'conflict', message: 'This board already has an answer.' };
        const labels = Array.isArray(args.labels) ? (args.labels as string[]) : [];
        const other = typeof args.other === 'string' && args.other.trim() ? args.other.trim() : null;
        if ((labels.length === 0) === (other === null)) throw { code: 'bad_args', message: 'Give exactly one of labels or other.' };
        b.answer = { labels, other, answeredAt: Date.now() };
        return boardState(id);
      }
      default:
        return undefined;
    }
  };
  return { prefixes: ['shown_gallery_get', 'shown_image_', 'option_board_state', 'option_board_answer'], handler };
}

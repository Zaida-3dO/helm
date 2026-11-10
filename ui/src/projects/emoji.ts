// The emoji the icon picker offers, and the rules for finding one and for taking a single emoji out
// of typed or pasted text. Pure, so node:test covers it.

export interface EmojiEntry {
  char: string;
  /** Lower-case search words. */
  words: string;
}

/** Two emoji joined into one with the zero-width joiner (a person and a tool). */
const joined = (a: number, b: number): string => String.fromCodePoint(a, 0x200d, b);

const RAW: [string, string][] = [
  // Faces and gestures
  ['😀', 'grin happy face'], ['😃', 'smile happy'], ['😄', 'laugh smile'], ['😁', 'beam grin'], ['😆', 'haha laugh'],
  ['😅', 'sweat smile nervous'], ['😂', 'joy tears laugh'], ['🙂', 'slight smile'], ['😉', 'wink'], ['😊', 'blush happy'],
  ['😇', 'angel halo'], ['😍', 'love heart eyes'], ['🤩', 'star struck wow'], ['😎', 'cool sunglasses'], ['🤓', 'nerd geek'],
  ['🧐', 'monocle curious'], ['🤔', 'thinking'], ['😴', 'sleep tired'], ['😮', 'surprised'], ['😱', 'scream shock'],
  ['🥳', 'party celebrate'], ['🤯', 'mind blown'], ['😭', 'cry sad'], ['😡', 'angry'], ['🤖', 'robot bot'],
  ['👻', 'ghost'], ['💀', 'skull'], ['👽', 'alien'], ['😺', 'cat smile'], ['🙈', 'monkey see no evil'],
  ['👍', 'thumbs up yes'], ['👏', 'clap applause'], ['🙌', 'hands raised'], ['🙏', 'pray thanks please'], ['💪', 'strong muscle'],
  ['👋', 'waving hello hi'], ['👀', 'eyes look'], ['🧠', 'brain mind'], ['👤', 'person user'], ['🦸', 'hero superhero'],
  ['🧙', 'wizard magic'],
  // Jobs and trades
  [joined(0x1f469, 0x1f4bb), 'woman technologist computer'], [joined(0x1f9d1, 0x1f4bb), 'developer programmer computer'],
  [joined(0x1f468, 0x1f373), 'cook chef'], [joined(0x1f9d1, 0x1f3a8), 'artist painter'], [joined(0x1f9d1, 0x1f680), 'astronaut space'],
  [joined(0x1f9d1, 0x1f3eb), 'teacher school'], [joined(0x1f9d1, 0x1f52c), 'scientist lab'], [joined(0x1f9d1, 0x1f33e), 'farmer garden'],
  // Animals
  ['🐶', 'dog puppy pet'], ['🐱', 'cat kitten pet'], ['🐭', 'mouse'], ['🐹', 'hamster'], ['🐰', 'rabbit bunny'],
  ['🦊', 'fox'], ['🐻', 'bear'], ['🐼', 'panda'], ['🐨', 'koala'], ['🐯', 'tiger'],
  ['🦁', 'lion'], ['🐮', 'cow'], ['🐷', 'pig'], ['🐸', 'frog'], ['🐵', 'monkey'],
  ['🐔', 'chicken'], ['🐧', 'penguin'], ['🐦', 'bird'], ['🦆', 'duck'], ['🦉', 'owl'],
  ['🦇', 'bat'], ['🐺', 'wolf'], ['🐴', 'horse'], ['🦄', 'unicorn'], ['🐝', 'bee honey'],
  ['🐛', 'caterpillar bug insect'], ['🦋', 'butterfly'], ['🐌', 'snail slow'], ['🐞', 'ladybug beetle'], ['🐜', 'ant'],
  ['🐢', 'turtle'], ['🐍', 'snake python'], ['🦎', 'lizard'], ['🐙', 'octopus'], ['🦑', 'squid'],
  ['🦀', 'crab'], ['🐠', 'fish'], ['🐬', 'dolphin'], ['🐳', 'whale'], ['🦈', 'shark'],
  ['🐘', 'elephant'], ['🦒', 'giraffe'], ['🐪', 'camel'], ['🐉', 'dragon'], ['🦖', 'dinosaur'],
  // Plants
  ['🌱', 'seedling sprout plant'], ['🌲', 'evergreen tree pine'], ['🌳', 'tree'], ['🌴', 'palm tree'], ['🌵', 'cactus desert'],
  ['🍀', 'clover luck'], ['🍁', 'maple leaf autumn'], ['🍄', 'mushroom'], ['🌸', 'blossom flower cherry'], ['🌹', 'rose flower'],
  ['🌻', 'sunflower flower'], ['🌷', 'tulip flower'], ['🌾', 'grain rice crop'], ['🪴', 'potted plant house plant'],
  // Sky and weather
  ['🌞', 'sun face'], ['🌙', 'moon crescent night'], ['⭐', 'star'], ['🌟', 'glowing star'], ['✨', 'sparkles shine'],
  ['⚡', 'lightning bolt power'], ['🔥', 'fire hot'], ['💧', 'droplet water'], ['🌊', 'ocean sea surf water'], ['⛄', 'snowman winter'],
  ['🌈', 'rainbow'], ['🌍', 'earth globe world'], ['🌋', 'volcano'], ['🌑', 'new moon dark'], ['🪐', 'planet saturn'],
  ['🌠', 'shooting star'],
  // Food and drink
  ['🍎', 'apple fruit'], ['🍊', 'orange fruit'], ['🍋', 'lemon fruit'], ['🍌', 'banana fruit'], ['🍉', 'watermelon fruit'],
  ['🍇', 'grapes fruit'], ['🍓', 'strawberry fruit'], ['🍒', 'cherries fruit'], ['🍑', 'peach fruit'], ['🥑', 'avocado'],
  ['🍅', 'tomato'], ['🥕', 'carrot vegetable'], ['🌽', 'corn'], ['🍞', 'bread bakery'], ['🧀', 'cheese'],
  ['🍳', 'egg cooking breakfast'], ['🥞', 'pancakes'], ['🍔', 'burger food'], ['🍟', 'fries food'], ['🍕', 'pizza food'],
  ['🌮', 'taco food'], ['🍣', 'sushi food'], ['🍜', 'noodles ramen food'], ['🍰', 'cake dessert'], ['🍪', 'cookie biscuit'],
  ['🍩', 'doughnut donut'], ['🍫', 'chocolate'], ['☕', 'coffee hot drink cafe'], ['🍵', 'tea drink'], ['🍺', 'beer drink'],
  ['🍷', 'wine drink'],
  // Games, sport and music
  ['⚽', 'soccer football sport'], ['🏀', 'basketball sport'], ['🏈', 'american football sport'], ['🎾', 'tennis sport'], ['🏆', 'trophy win'],
  ['🎯', 'target goal bullseye'], ['🎮', 'game controller play'], ['🎲', 'dice game'], ['🧩', 'puzzle piece'], ['🎨', 'paint palette art'],
  ['🎭', 'theatre drama masks'], ['🎬', 'film clapper movie'], ['🎤', 'microphone sing'], ['🎧', 'headphones audio'], ['🎵', 'music note'],
  ['🎸', 'guitar music'], ['🎹', 'piano keyboard music'], ['🥁', 'drum music'], ['🎷', 'saxophone music'], ['🎺', 'trumpet music'],
  // Travel and places
  ['🚗', 'car drive'], ['🚕', 'taxi cab'], ['🚌', 'bus'], ['🚂', 'train rail'], ['🚀', 'rocket launch space'],
  ['🛸', 'ufo flying saucer'], ['🚲', 'bicycle bike'], ['🛵', 'scooter'], ['⛵', 'sailboat boat sail'], ['🚢', 'ship boat'],
  ['🚁', 'helicopter'], ['🏠', 'house home'], ['🏢', 'office building work'], ['🏫', 'school'], ['🏥', 'hospital health'],
  ['🏭', 'factory industry'], ['🏰', 'castle'], ['⛺', 'tent camping'], ['🗼', 'tower'], ['🗽', 'statue liberty'],
  ['🌆', 'city dusk'], ['🌉', 'bridge night'],
  // Devices, office and tools
  ['💻', 'laptop computer'], ['📱', 'phone mobile'], ['📷', 'camera photo'], ['📺', 'television tv'], ['📻', 'radio'],
  ['🔋', 'battery power'], ['🔌', 'plug electric'], ['💡', 'idea light bulb'], ['🔦', 'flashlight torch'], ['📚', 'books library'],
  ['📖', 'open book read'], ['📝', 'memo note write'], ['📄', 'page document'], ['📁', 'folder files'], ['📂', 'open folder files'],
  ['📌', 'pin pushpin'], ['📎', 'paperclip attach'], ['📅', 'calendar date'], ['📆', 'tear off calendar date'], ['📈', 'chart up growth'],
  ['📉', 'chart down decline'], ['📊', 'bar chart stats'], ['📋', 'clipboard list'], ['📦', 'package box parcel'], ['📬', 'mailbox post'],
  ['📧', 'email mail'], ['📢', 'loudspeaker announce'], ['🔔', 'bell notify'], ['🔍', 'magnifier search find'], ['🔒', 'lock secure'],
  ['🔑', 'key'], ['🔧', 'wrench tool'], ['🔨', 'hammer tool build'], ['🧰', 'toolbox tools'], ['🧪', 'test tube lab experiment'],
  ['🔬', 'microscope science'], ['🔭', 'telescope space'], ['💊', 'pill medicine'], ['💉', 'syringe'], ['🧬', 'dna genetics'],
  ['💰', 'money bag cash'], ['💳', 'credit card payment'], ['💎', 'gem diamond'], ['🛒', 'shopping cart'], ['🎁', 'gift present'],
  ['🎈', 'balloon party'], ['🎉', 'party popper celebrate'], ['🧲', 'magnet'], ['🧭', 'compass navigate'], ['⏰', 'alarm clock time'],
  ['⌛', 'hourglass time'], ['🕐', 'clock time'], ['🧱', 'brick wall build'], ['🪑', 'chair seat'], ['🚪', 'door'],
  ['🧹', 'broom clean'], ['🧺', 'basket'], ['🧴', 'bottle lotion'], ['💼', 'briefcase work business'], ['🎒', 'backpack school bag'],
  ['👓', 'glasses'], ['🧢', 'cap hat'], ['👑', 'crown king'], ['💍', 'ring'], ['📡', 'satellite antenna signal'],
  ['🔗', 'link chain'],
  // Hearts and symbols
  ['💙', 'blue heart'], ['💚', 'green heart'], ['💛', 'yellow heart'], ['🧡', 'orange heart'], ['💜', 'purple heart'],
  ['🖤', 'black heart'], ['🤍', 'white heart'], ['🤎', 'brown heart'], ['💖', 'sparkling heart love'], ['💔', 'broken heart'],
  ['✅', 'check done yes'], ['❌', 'cross no wrong'], ['❓', 'question'], ['❗', 'exclamation important'], ['⭕', 'circle'],
  ['🔴', 'red circle'], ['🟠', 'orange circle'], ['🟡', 'yellow circle'], ['🟢', 'green circle'], ['🔵', 'blue circle'],
  ['🟣', 'purple circle'], ['⚫', 'black circle'], ['⚪', 'white circle'], ['🟤', 'brown circle'], ['🔶', 'orange diamond'],
  ['🔷', 'blue diamond'], ['🔺', 'red triangle'], ['🟥', 'red square'], ['🟧', 'orange square'], ['🟨', 'yellow square'],
  ['🟩', 'green square'], ['🟦', 'blue square'], ['🟪', 'purple square'], ['💯', 'hundred perfect'], ['🆕', 'new'],
  ['🆒', 'cool'], ['🔝', 'top'], ['🚩', 'red flag'], ['🏁', 'chequered flag finish race'], ['💬', 'speech chat talk'],
  ['💭', 'thought bubble'], ['🔰', 'beginner'], ['➕', 'plus add'], ['➖', 'minus'], ['➗', 'divide'],
  ['🔀', 'shuffle'], ['🔁', 'repeat loop'], ['🔄', 'refresh sync'], ['⏩', 'fast forward'],
];

export const EMOJI: EmojiEntry[] = RAW.map(([char, words]) => ({ char, words }));

/** The emoji whose words (or character) hold every word typed; an empty query gives them all. */
export function searchEmoji(list: EmojiEntry[], query: string): EmojiEntry[] {
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (terms.length === 0) return list;
  return list.filter((e) => terms.every((t) => e.words.includes(t) || e.char === t));
}

interface SegmenterLike {
  segment(text: string): Iterable<{ segment: string }>;
}

function graphemes(text: string): string[] {
  const Seg = (Intl as unknown as { Segmenter?: new (locale?: string, o?: { granularity: 'grapheme' }) => SegmenterLike }).Segmenter;
  if (Seg) return Array.from(new Seg(undefined, { granularity: 'grapheme' }).segment(text), (s) => s.segment);
  // Without the segmenter: join code points into joined sequences, flag pairs and marks.
  const out: string[] = [];
  let joined = false;
  for (const ch of Array.from(text)) {
    const cp = ch.codePointAt(0) ?? 0;
    const mark = cp === 0xfe0f || (cp >= 0x1f3fb && cp <= 0x1f3ff) || cp === 0x20e3;
    const flag = cp >= 0x1f1e6 && cp <= 0x1f1ff;
    const last = out[out.length - 1];
    const lastFlags = last ? Array.from(last).filter((c) => (c.codePointAt(0) ?? 0) >= 0x1f1e6 && (c.codePointAt(0) ?? 0) <= 0x1f1ff).length : 0;
    if (last && (joined || mark || (flag && lastFlags === 1 && Array.from(last).length === 1))) {
      out[out.length - 1] = last + ch;
    } else {
      out.push(ch);
    }
    joined = cp === 0x200d;
  }
  return out;
}

const PICTOGRAPH = /\p{Extended_Pictographic}|\p{Regional_Indicator}/u;

/** The first single emoji in typed or pasted text: a joined sequence or a flag counts as one. Null when there is none. */
export function firstEmoji(text: string): string | null {
  for (const g of graphemes(text)) {
    if (g.trim() === '' || /[A-Za-z0-9]/.test(g)) continue;
    if (PICTOGRAPH.test(g)) return g;
  }
  return null;
}

// `chatElements`: four transcript parts the user can hide. Lenient on read (anything
// unknown is dropped, a missing key is shown); the host validates writes strictly.

export type ElementId = 'tokens' | 'turnTime' | 'fileChanges' | 'timeline';
export type ChatElements = Partial<Record<ElementId, 'shown' | 'hidden'>>;

export const ELEMENTS: { id: ElementId; title: string; description: string }[] = [
  { id: 'tokens', title: 'Token counts', description: 'The token count under each reply.' },
  { id: 'turnTime', title: 'Turn time', description: 'How long each reply took.' },
  { id: 'fileChanges', title: 'File changes', description: 'The cards listing the files a turn changed.' },
  { id: 'timeline', title: 'Work timeline', description: 'The rail showing what the agent worked on.' },
];

export function parseChatElements(v: unknown): ChatElements {
  const out: ChatElements = {};
  if (!v || typeof v !== 'object') return out;
  for (const { id } of ELEMENTS) {
    const x = (v as Record<string, unknown>)[id];
    if (x === 'shown' || x === 'hidden') out[id] = x;
  }
  return out;
}

export const isShown = (e: ChatElements, id: ElementId): boolean => e[id] !== 'hidden';

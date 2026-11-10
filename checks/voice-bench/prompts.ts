// The benchmark's 20 prompts. All of them are synthetic: made-up questions and made-up status
// text, with every fact the answer needs inside the prompt itself, so a chat never has to reach
// for a tool (which, in a benchmark run, would stop at an approval nobody gives).
//
// `turn` prompts arrive the way a spoken turn from the phone does. `news` prompts arrive the way
// a message from another chat does, which is how the voice chat is woken for things it was not
// asked about; the chat should either tell the user (`expect: "tell"`) or answer exactly
// `[quiet]` (`expect: "quiet"`). `quick` marks the seven that make up a cheap trial run.

export type PromptKind = "turn" | "news";

export interface Prompt {
  id: string;
  kind: PromptKind;
  /** What the prompt is meant to try, in a few words. */
  tests: string;
  text: string;
  /** News only: what a good chat does with it. */
  expect?: "tell" | "quiet";
  /** A turn that carries on the one before it, as the phone's one-second rejoin does. */
  continues?: boolean;
  quick?: boolean;
}

export const PROMPTS: Prompt[] = [
  { id: "p01", kind: "turn", quick: true, tests: "short question", text: "What's the capital of Australia, and is it also the biggest city?" },
  { id: "p02", kind: "turn", tests: "quick opinion", text: "Should I use tabs or spaces in a brand new project? Just give me your quick take." },
  {
    id: "p03",
    kind: "turn",
    quick: true,
    tests: "status of another chat",
    text: "What is the build doing right now? The build chat's last message says: ran 212 tests, 209 passed, 3 are still running, and the linter reported 2 warnings in the settings module.",
  },
  {
    id: "p04",
    kind: "turn",
    quick: true,
    tests: "long result to summarise",
    text:
      "Summarise this for me. The nightly job started at two in the morning and checked out the main branch. " +
      "It compiled the core library in four minutes, then the command line tool in two minutes, then the desktop app in nine minutes. " +
      "The unit tests ran for eleven minutes: 1,204 passed, 6 were skipped because they need a graphics card, and 2 failed. " +
      "One failure was in the date parser, which rejects the 29th of February in leap years. " +
      "The other was in the cache, where an entry expired a few milliseconds too early on the slow machine. " +
      "The integration tests then ran for twenty-two minutes against a fake payments service and passed. " +
      "The packaging step produced three installers, and the signing step failed because the certificate expires in six days, " +
      "which is a warning rather than an error. " +
      "The job finished at three-oh-one, uploaded the installers to the staging bucket, and posted a link to the team channel.",
  },
  { id: "p05", kind: "turn", tests: "steps (a list)", text: "Give me the steps to start a new project folder with git, a readme and a licence." },
  {
    id: "p06",
    kind: "turn",
    quick: true,
    tests: "a link, an id and a number",
    text: "Tell me what this is about: https://example.com/builds/9f8e7d6c5b4a/artifacts?run=48213 is the artifact page for pull request #1234, commit 3fa9c1d.",
  },
  { id: "p07", kind: "turn", tests: "tempts code", text: "How do I reverse a string in Python?" },
  {
    id: "p08",
    kind: "turn",
    quick: true,
    tests: "tempts a table",
    text: "Compare white bread, wholemeal bread and sourdough on calories per slice, fibre and price, in a table.",
  },
  { id: "p09", kind: "turn", tests: "tempts file paths", text: "Where does a typical Linux system keep its logs, and which file holds the login attempts?" },
  { id: "p10", kind: "turn", tests: "yes or no with a catch", text: "Is it safe to restart a database while a backup is running?" },
  {
    id: "p11",
    kind: "turn",
    tests: "advice with numbers",
    text: "I have forty minutes before a meeting. How should I split the time between reviewing a document and answering messages?",
  },
  { id: "p12", kind: "turn", tests: "long explanation (the cap)", text: "Explain how a hash map works." },
  { id: "p13", kind: "turn", continues: true, tests: "follow-up in the same conversation", text: "And what happens when two keys collide?" },
  {
    id: "p14",
    kind: "turn",
    tests: "numbers read aloud",
    text: "Read me these test results and tell me which area to look at first: auth 41 passed 0 failed, billing 37 passed 2 failed, search 58 passed 1 failed.",
  },
  { id: "p15", kind: "turn", tests: "too little to go on", text: "Um, so, the thing from yesterday, can you remind me what we decided about it?" },
  {
    id: "p16",
    kind: "news",
    expect: "tell",
    quick: true,
    tests: "good news worth telling",
    text: "News from the build chat: the nightly build finished and all 212 tests passed after the settings warning was fixed.",
  },
  {
    id: "p17",
    kind: "news",
    expect: "tell",
    tests: "a problem that needs a decision",
    text: "News from the deploy chat: the staging deploy failed at the database migration step and it needs a decision about rolling back.",
  },
  {
    id: "p18",
    kind: "news",
    expect: "quiet",
    quick: true,
    tests: "routine, nothing needed",
    text: "News from the docs chat: it finished reformatting the table of contents. Nothing is needed from anyone.",
  },
  { id: "p19", kind: "news", expect: "quiet", tests: "routine start", text: "News from the tests chat: a routine lint run has started." },
  {
    id: "p20",
    kind: "news",
    expect: "tell",
    tests: "blocked on the user",
    text: "News from the research chat: it found two conflicting answers and wants you to pick one before it carries on.",
  },
];

/** The prompts a run uses: all of them, or the seven marked `quick`. */
export function choose(quick: boolean): Prompt[] {
  return quick ? PROMPTS.filter((p) => p.quick) : PROMPTS;
}

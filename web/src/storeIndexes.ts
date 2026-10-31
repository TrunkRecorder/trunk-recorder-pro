// What the store's call indexes are for, in a sentence each: Setup → Data
// says it beside the switches for the two optional ones, Platform → Storage
// beside each index's size. (crates/trunk-pro/src/db/schema.rs INDEXES.)

/** An index of the call store: what it finds, and who asks for that. */
export interface IndexUse {
  /** What it finds, briefly. */
  finds: string;
  /** Where that's used. */
  used: string;
  /** Its setting, for the two that can be turned off. */
  setting?: "radioIndex" | "talkgroupIndex";
  /** What's shown without it. */
  without?: string;
}

const ASKED = "interfaces, plugins and tools that ask the recorder for calls";

export const INDEX_USES: Record<string, IndexUse> = {
  calls_start: { finds: "every call, newest first", used: "the Listen page's calls and Load older, and the call list for " + ASKED },
  calls_system_start: { finds: "a system's calls, newest first", used: "a system's call list (the Radio page, and " + ASKED + ")" },
  calls_system_tg_start: {
    finds: "a talkgroup's calls, newest first",
    used: "the Radio page's calls when you pick a talkgroup, and " + ASKED + " by talkgroup",
    setting: "talkgroupIndex",
    without: "a talkgroup's list is its recent calls only",
  },
  transmissions_radio: {
    finds: "the calls a radio talked on, newest first",
    used: "the Radio page's calls when you pick a radio, and " + ASKED + " by radio",
    setting: "radioIndex",
    without: "a radio's list is its recent calls only",
  },
};

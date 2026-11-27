# Page alerts — a Trunk Recorder Pro plugin

Takes over from Tone Board. Trunk Recorder Pro already finds the tones in every call it records
(two-tone pages, long alert tones, beeps). This plugin names each page by your agency list —
Tone Board's, with its timing rules — speaks them here, and sends the call to Rdio Scanner under
the agency's talkgroup, as Tone Board does.

## What it does

- **Names pages** on the channels you list (each a TRP talkgroup, such as `UHF/1` for DC 911),
  by your agencies: tone A and B within the tolerance (1.5 % by default), A/B lengths and gap when
  set (`1+` = at least 1 s), single long tones, and beeps (the fewest that count). The closest
  agency wins. Pages that match no agency are logged as UNKNOWN.
- **Speak**: each tone set (and Multi-Station row) has a Speak switch. When it's paged, its sound
  (`beep`, or a file in the sounds folder) plays and its name is spoken on this computer. The
  **Speak on this computer** switch silences it all. (0.2.0 dropped On duty / Off duty: entries
  that alerted at any time now have Speak on.)
- **Join the dispatch after**: when the carrier drops between the tones and the dispatcher, TRP
  records two calls. A page waits until its channel has been quiet for this many seconds
  (default 15; 0 is off), and every call that started on the channel in that time goes up in
  the page's recording, after a half-second pause. A new page on the channel sends the one
  before right away. It needs ffmpeg for an .m4a (else it uploads a WAV).
- **Fill timings**: fills a tone set's empty A length, gap and B length from the pages of it in
  TRP's recordings (the last 120 days). A length only goes in when every page heard agrees with
  it; a long tone gets "at least" the shortest heard. The button at the top does every tone set.
- **Multi-Station**: a row alerts when all its agencies are paged in one page (several
  transmissions, while the channel isn't quiet for the follow-on window, count as one).
- **Counties on systems of their own**: a channel's **Rdio Scanner system** sends its pages to that
  system (Ulster's, Orange's) instead of the main one. **Add from CSV** adds a county's tone sets
  (Tone Board's CSV columns) from the CSV named in **Import from** to the list here; tone sets
  already here, by name, are left as they are. (Import from Tone Board replaces the list.)
- **Save unknown pages**: a page whose tones match no tone set is kept to listen to later, whether
  or not it's sent: its recording (with the dispatch after it joined on) goes in
  `~/page-alerts-unknown/<day>/` as `<time> <channel> <tones>.m4a`, a `.txt` beside it saying what
  was heard (and which tone sets were paged in the same call), and a line in `index.csv` there.
  Days older than **Keep unknown pages, days** (60) are deleted.
- **Send its pages** (each channel): that channel's pages go to Rdio Scanner even while Send pages
  to Rdio Scanner is off, so counties Tone Board doesn't send can go up from here first.
- **Send pages to Rdio Scanner** (off at first): with the server, API key and system ID filled in
  and this off, only the Test buttons send, so you can test while Tone Board still uploads. At the
  cut-over, turn it on and turn Tone Board's uploads off.
- **Rdio Scanner, once per page**: a page (every tone set in it, and the dispatch after) goes up
  once, on one talkgroup, with every other talkgroup it's on as a patch: each tone set's Unit ID
  ("each tone set is its own talkgroup"), the channel's for unknown tones and tone sets without a
  Unit ID, and its Multi-Station rows'. The main talkgroup is the Multi-Station row's when one
  fired, else the first tone set's. Someone listening to any or all of them hears it once. All of
  these talkgroups, the Multi-Station rows' included, are in the one pages system (patches only
  reach talkgroups of the call's own system). The **Test** on a Multi-Station row sends one such
  call. Unknown tones under the channel's talkgroup. Retries after 10 s, 30 s, 2 min, 10 min;
  waiting uploads survive a restart.
- **Test** on each agency: speaks "This is a Trunk Recorder Pro alert test for …" here, and sends
  it to Rdio Scanner where that agency's pages go.
- Every page is logged (the plugin's log, and `pages.jsonl` in its data folder).

## Setting it up

1. `bash build.sh`, then on TRP's **Plugins** page, **Add from a file…** →
   `~/trp-plugins/page-alerts/page-alerts`.
2. Open its **Pages** page. **Import from** is Tone Board's folder (`~/.local/share/toneboard`):
   press **Import from Tone Board**. It brings the channels that follow a TRP talkgroup, the agency
   list Tone Board uses, its Multi-Station rows, and its Rdio Scanner choice (each agency its own
   talkgroup).
3. Rdio Scanner: the server, API key and Tone Board's system ID. Leave the server empty to try it
   first without sending anything.
4. **Test the voice**, and **Test** on an agency.

Channels Tone Board took from OpenMHz (FDNY) aren't TRP's, so their agencies aren't matched here;
the Queens recorder, with this plugin, will hear them.

#!/usr/bin/env python3
"""Render zeffy/campaign.toml into campaign-kit.html: every field of the
Zeffy form builder, in builder order, with copy buttons.

Zeffy's API can't create forms, so a person builds the campaign by hand;
this page makes that a copy-paste job that can't drift from the spec.

    python3 zeffy/render_kit.py            # writes zeffy/campaign-kit.html
"""
import base64
import html
import pathlib
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent
spec = tomllib.loads((ROOT / "zeffy/campaign.toml").read_text())
c = spec["campaign"]


def data_uri(rel):
    return "data:image/png;base64," + base64.b64encode((ROOT / rel).read_bytes()).decode()


def esc(s):
    return html.escape(str(s).strip())


def flow(text):
    """TOML '\\' line continuations are already joined; keep paragraphs."""
    return "\n\n".join(p.strip() for p in str(text).strip().split("\n\n"))


def copyable(label, value, multiline=False, note=""):
    tag = "pre" if multiline else "code"
    return f"""
      <div class="field">
        <div class="field-head"><span class="field-label">{esc(label)}</span>
          <button class="copy" type="button">Copy</button></div>
        <{tag} class="value">{esc(value)}</{tag}>
        {f'<p class="note">{note}</p>' if note else ''}
      </div>"""


def setting(label, on, detail=""):
    state = "on" if on else "off"
    return f"""<li class="setting"><span class="toggle {state}">{'On' if on else 'Off'}</span>
      <span>{esc(label)}{f' <span class="muted">· {esc(detail)}</span>' if detail else ''}</span></li>"""


steps = []


def step(title, body, sub=""):
    n = len(steps) + 1
    steps.append((n, title, f"""
    <section class="step" id="step-{n}">
      <label class="step-head" for="done-{n}">
        <input type="checkbox" id="done-{n}" class="done" data-step="{n}">
        <span class="step-num">Step {n}</span>
        <h2>{esc(title)}</h2>
      </label>
      {f'<p class="step-sub">{sub}</p>' if sub else ''}
      <div class="step-body">{body}</div>
    </section>"""))


step("Campaign title",
     copyable("Title", c["title"], note="The form URL is set from this at first save and <strong>can't change later</strong>. Type it exactly."),
     "Campaigns → Create a campaign → Donation form.")
step("Fundraising goal",
     copyable("Goal (USD)", f'{c["goal_usd"]:,}'.replace(",", ""),
              note="CI checks this matches <code>goal_usd</code> in the spec."),
     "Where the money goes:" + "<table class='budget'>" + "".join(
         f"<tr><td>{esc(b['fund'])}</td><td class='num'>${b['usd']:,}</td><td class='muted'>{esc(b['detail'])}</td></tr>"
         for b in spec["budget"]) + f"<tr class='total'><td>Total</td><td class='num'>${c['goal_usd']:,}</td><td></td></tr></table>")
d = spec["description"]
step("Description",
     copyable("Headline", d["headline"]) + copyable("Body", flow(d["body"]), multiline=True,
                                                    note="Paste into Zeffy's description editor; bold the program names if the editor drops the ** markers."))
a = spec["appearance"]
step("Colours, banner and logo", f"""
      <ul class="settings">{setting('Theme: ' + a['theme'], True)}{setting('Background shapes', a['shapes'])}</ul>
      <figure class="art"><img src="{data_uri(a['banner'])}" alt="Grow the Valley banner: goats, chickens, seedlings and a cat under a rising sun">
        <figcaption>Banner (16:9, no text: Zeffy shows the title). In the repo at <code>{esc(a['banner'])}</code>.</figcaption></figure>
      <figure class="art logo"><img src="{data_uri('brand/mark-256.png')}" alt="Sun Brewed Homestead mark: a sprout under a rising sun">
        <figcaption>Logo. Full size at <code>{esc(a['logo'])}</code>.</figcaption></figure>""",
     "Right-click an image and choose Save Image to upload it.")
g = spec["giving"]
freq_names = {"one_time": "One time", "monthly": "Monthly", "quarterly": "Quarterly", "yearly": "Yearly"}
amount_html = ""
for f in g["frequencies"]:
    rows = "".join(
        f"<tr><td class='num'>${x['usd']}</td><td>{esc(x['impact'])}</td><td><button class='copy mini' type='button' data-copy='{esc(x['impact'])}'>Copy</button></td></tr>"
        for x in g[f])
    amount_html += f"<h3>{freq_names[f]}</h3><table class='amounts'>{rows}</table>"
step("Amounts and frequencies",
     f"<p>Turn on <strong>{', '.join(freq_names[f] for f in g['frequencies'])}</strong>. Default: <strong>{freq_names[g['default_frequency']]}, ${g['default_amount_usd']}</strong>.</p>" + amount_html,
     "Each impact line appears when a donor taps that amount.")
step("Tax receipts", f"<ul class='settings'>{setting('Automatic tax receipts', spec['receipts']['tax_receipts'], 'Sun Brewed Homestead Inc., EIN 92-0510478')}</ul>")
qs = ""
for q in spec["questions"]:
    kind = {"fund_designation": "Fund designation", "single_select": "Single choice", "multi_select": "Multiple choice"}.get(q["kind"], q["kind"])
    opts = "".join(f"<li>{esc(o)}{' <span class=\"pill\">default</span>' if q.get('default') == o else ''}</li>" for o in q["options"])
    qs += f"""<div class="question"><div class="q-meta">{kind} · {'required' if q['required'] else 'optional'}</div>
      {copyable('Question', q['label'])}<ul class="options">{opts}</ul></div>"""
step("Custom questions", qs, "Zeffy already asks for name, email, state and country.")
e = spec["thank_you_email"]
step("Thank-you email", copyable("Subject", e["subject"]) + copyable("Message", e["body"].strip(), multiline=True,
     note="Swap <code>{first_name}</code> for Zeffy's first-name merge tag."))
adv = spec["advanced"]
step("Advanced settings", f"""<ul class="settings">
      {setting('Honorary and memorial gifts', adv['honorary_donations'])}
      {setting('Show donor list publicly', adv['public_donor_list'], 'donors stay private')}
      {setting('Progress thermometer', adv['thermometer'])}
      {setting('Accept cheques over $1,000', adv['check_donations'])}
      {setting('Spanish translation', 'es' in c.get('translations', []), 'have a native speaker review it')}
    </ul>""")
step("Keep it private", f"""<p>Save the form and <strong>do not publish it</strong>. Draft forms have no public link.
      The spec says <code>status = "{esc(c['status'])}"</code>, and the nightly check fails if the campaign goes live before that line changes.</p>
      <p>While you're in Zeffy: the starter form <strong>Donate to Change Lives</strong> is currently live with a $1,000,000 goal. Archive it unless you want it.</p>""")

nav = "".join(f'<li><a href="#step-{n}"><span class="dot" data-dot="{n}"></span>{esc(t)}</a></li>' for n, t, _ in steps)
body = "".join(s for _, _, s in steps)

page = f"""<title>Grow the Valley Kit</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Young+Serif&family=Figtree:wght@400;500;600;700&family=IBM+Plex+Mono:wght@400;500&display=swap">
<style>
:root {{
  --ground: #FFF8EC; --paper: #FFFFFF; --ink: #33210F; --muted: #7A6552; --line: #EBDDC6;
  --sun: #E2963B; --sun-ink: #8A4E0A; --sprout: #3D7A3A; --sprout-soft: #E4F0DC; --soil: #5B3A22;
  --display: "Young Serif", Georgia, serif; --body: "Figtree", system-ui, sans-serif; --mono: "IBM Plex Mono", ui-monospace, monospace;
}}
@media (prefers-color-scheme: dark) {{ :root:not([data-theme="light"]) {{
  color-scheme: dark; --ground: #1C150E; --paper: #261C13; --ink: #F5E9D6; --muted: #B79F84; --line: #3B2E21;
  --sun: #F0A94F; --sun-ink: #F6C27D; --sprout: #8CCB5E; --sprout-soft: #22301B; --soil: #D9B48E; }} }}
:root[data-theme="dark"] {{
  color-scheme: dark; --ground: #1C150E; --paper: #261C13; --ink: #F5E9D6; --muted: #B79F84; --line: #3B2E21;
  --sun: #F0A94F; --sun-ink: #F6C27D; --sprout: #8CCB5E; --sprout-soft: #22301B; --soil: #D9B48E; }}
* {{ box-sizing: border-box; }}
img {{ max-width: 100%; height: auto; }}
main > *, .step-body > * {{ min-width: 0; }}
body {{ background: var(--ground); color: var(--ink); font: 16px/1.6 var(--body); padding-inline: 16px; }}
.wrap {{ max-width: 1080px; margin: 0 auto; padding-block: 32px 64px; display: grid; grid-template-columns: 240px minmax(0, 1fr); gap: 40px; }}
header.hero {{ grid-column: 1 / -1; display: flex; gap: 20px; align-items: center; }}
header.hero img {{ width: 72px; height: 72px; border-radius: 50%; flex: none; }}
.eyebrow {{ font-size: 12px; letter-spacing: .16em; text-transform: uppercase; color: var(--sun-ink); font-weight: 600; margin: 0; }}
h1 {{ font: 400 clamp(32px, 5vw, 48px)/1.05 var(--display); margin: 4px 0 6px; text-wrap: balance; }}
.lede {{ margin: 0; color: var(--muted); max-width: 62ch; }}
.status {{ display: inline-flex; gap: 6px; align-items: center; font-size: 13px; font-weight: 600; color: var(--sprout);
  background: var(--sprout-soft); border-radius: 999px; padding: 2px 10px; margin-top: 10px; }}
nav {{ position: sticky; top: calc(env(safe-area-inset-top, 0px) + 20px); align-self: start; }}
nav ol {{ list-style: none; padding: 0; margin: 0; display: grid; gap: 2px; }}
nav a {{ display: flex; gap: 10px; align-items: center; color: var(--ink); text-decoration: none; padding: 6px 8px; border-radius: 6px; font-size: 14px; }}
nav a:hover, nav a:focus-visible {{ background: var(--paper); outline: none; }}
.dot {{ width: 10px; height: 10px; border-radius: 50%; border: 2px solid var(--line); flex: none; }}
.dot.done {{ background: var(--sprout); border-color: var(--sprout); }}
.progress {{ font-size: 13px; color: var(--muted); margin: 14px 8px 0; font-variant-numeric: tabular-nums; }}
main {{ display: grid; gap: 22px; min-width: 0; }}
.step {{ background: var(--paper); border: 1px solid var(--line); border-radius: 14px; padding: 22px 24px; scroll-margin-top: 20px; }}
.step-head {{ display: grid; grid-template-columns: auto 1fr; column-gap: 12px; align-items: center; cursor: pointer; }}
.step-head input {{ grid-row: span 2; width: 22px; height: 22px; accent-color: var(--sprout); }}
.step-num {{ font-size: 12px; letter-spacing: .12em; text-transform: uppercase; color: var(--muted); font-weight: 600; }}
h2 {{ font: 400 24px/1.2 var(--display); margin: 0; }}
h3 {{ font: 600 14px/1 var(--body); letter-spacing: .08em; text-transform: uppercase; color: var(--muted); margin: 18px 0 6px; }}
.step-sub {{ color: var(--muted); margin: 8px 0 0 34px; }}
.step-body {{ margin-top: 14px; display: grid; gap: 14px; }}
.step.is-done {{ border-color: var(--sprout); }}
.step.is-done h2 {{ color: var(--sprout); }}
.field {{ border: 1px solid var(--line); border-radius: 10px; overflow: hidden; }}
.field-head {{ display: flex; justify-content: space-between; align-items: center; padding: 8px 12px; border-bottom: 1px solid var(--line); }}
.field-label {{ font-size: 13px; font-weight: 600; color: var(--muted); }}
.value {{ display: block; margin: 0; padding: 12px; font: 14px/1.6 var(--mono); white-space: pre-wrap; word-break: break-word; }}
.note {{ margin: 0; padding: 0 12px 10px; font-size: 13px; color: var(--muted); }}
button.copy {{ font: 600 13px var(--body); color: var(--sun-ink); background: transparent; border: 1px solid var(--sun); border-radius: 6px; padding: 3px 10px; cursor: pointer; }}
button.copy:hover {{ background: var(--sun); color: #2A1706; }}
button.copy:focus-visible {{ outline: 2px solid var(--sun); outline-offset: 2px; }}
button.copy.copied {{ background: var(--sprout); border-color: var(--sprout); color: #fff; }}
button.mini {{ padding: 1px 8px; font-size: 12px; }}
table {{ width: 100%; border-collapse: collapse; font-size: 15px; }}
td {{ padding: 8px 6px; border-bottom: 1px solid var(--line); vertical-align: top; }}
td.num {{ font-variant-numeric: tabular-nums; font-weight: 600; white-space: nowrap; }}
tr.total td {{ border-bottom: 0; font-weight: 700; }}
.budget, .amounts {{ margin-top: 6px; }}
.muted {{ color: var(--muted); }}
.settings {{ list-style: none; padding: 0; margin: 0; display: grid; gap: 8px; }}
.setting {{ display: flex; gap: 10px; align-items: baseline; }}
.toggle {{ font-size: 12px; font-weight: 700; border-radius: 999px; padding: 1px 9px; min-width: 38px; text-align: center; }}
.toggle.on {{ background: var(--sprout-soft); color: var(--sprout); }}
.toggle.off {{ background: var(--line); color: var(--muted); }}
.art {{ margin: 0; }}
.art img {{ border-radius: 10px; border: 1px solid var(--line); display: block; }}
.art.logo img {{ width: 96px; border-radius: 50%; }}
figcaption {{ font-size: 13px; color: var(--muted); margin-top: 6px; }}
.question {{ display: grid; gap: 8px; }}
.q-meta {{ font-size: 13px; color: var(--muted); font-weight: 600; }}
.options {{ margin: 0; padding-left: 20px; }}
.pill {{ font-size: 11px; font-weight: 700; color: var(--sun-ink); border: 1px solid var(--sun); border-radius: 999px; padding: 0 7px; }}
code {{ font-family: var(--mono); font-size: .92em; }}
@media (max-width: 760px) {{ .wrap {{ grid-template-columns: 1fr; gap: 20px; }} nav {{ position: static; }} .step {{ padding: 18px 16px; }} .step-sub {{ margin-left: 0; }} }}
@media (prefers-reduced-motion: no-preference) {{ html {{ scroll-behavior: smooth; }} }}
</style>
<div class="wrap">
  <header class="hero">
    <img src="{data_uri('brand/mark-256.png')}" alt="">
    <div>
      <p class="eyebrow">Sun Brewed Homestead · Zeffy campaign kit</p>
      <h1>{esc(c['title'])}</h1>
      <p class="lede">Everything needed to build our ${c['goal_usd']:,} campaign in Zeffy, in the order Zeffy's builder asks for it. Generated from <code>zeffy/campaign.toml</code>; change the spec, not this page.</p>
      <span class="status">● Private until launch (Draft)</span>
    </div>
  </header>
  <nav aria-label="Builder steps"><ol>{nav}</ol><p class="progress" id="progress"></p></nav>
  <main>{body}</main>
</div>
<script>
(() => {{
  const KEY = "gtv-kit-done";
  let done = {{}};
  try {{ done = JSON.parse(localStorage.getItem(KEY) || "{{}}"); }} catch (_) {{}}
  const boxes = [...document.querySelectorAll(".done")];
  const paint = () => {{
    let n = 0;
    for (const b of boxes) {{
      const s = b.dataset.step, on = !!done[s];
      b.checked = on; if (on) n++;
      b.closest(".step").classList.toggle("is-done", on);
      document.querySelector(`[data-dot="${{s}}"]`).classList.toggle("done", on);
    }}
    document.getElementById("progress").textContent = `${{n}} of ${{boxes.length}} steps done`;
  }};
  for (const b of boxes) b.addEventListener("change", () => {{
    done[b.dataset.step] = b.checked;
    try {{ localStorage.setItem(KEY, JSON.stringify(done)); }} catch (_) {{}}
    paint();
  }});
  paint();
  document.addEventListener("click", async (ev) => {{
    const btn = ev.target.closest("button.copy");
    if (!btn) return;
    const text = btn.dataset.copy ?? btn.closest(".field").querySelector(".value").textContent;
    try {{ await navigator.clipboard.writeText(text); }}
    catch (_) {{
      const el = btn.dataset.copy ? btn.closest("tr").children[1] : btn.closest(".field").querySelector(".value");
      const r = document.createRange(); r.selectNodeContents(el);
      const sel = getSelection(); sel.removeAllRanges(); sel.addRange(r);
      btn.textContent = "Selected"; setTimeout(() => (btn.textContent = "Copy"), 1600); return;
    }}
    btn.textContent = "Copied"; btn.classList.add("copied");
    setTimeout(() => {{ btn.textContent = "Copy"; btn.classList.remove("copied"); }}, 1600);
  }});
}})();
</script>
"""
(ROOT / "zeffy/campaign-kit.html").write_text(page)
print("wrote zeffy/campaign-kit.html")

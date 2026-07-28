#!/usr/bin/env python3
# smol · Control Room builder — MINIMAL FIX: un-nest the fleet (the black hole).
# JP screenshot: node cards were MISSING because they lived in a nested custom:grid-layout
# card that renders EMPTY. Fix = splice node cards DIRECTLY into the view grid (span 4, like
# glass/power/forge which render). Node cards stay LIVE mushroom boxes (header + OLED + entities).
# If mushroom still doesn't render un-nested → it's mushroom, swap to SVG faceplate then.
#   HA_TOKEN=$(cat ~/.cache/ha-token-tmp) python3 build_control_room.py
#   (HA_WS_URI / HA_SSH override the endpoints; the defaults are this homelab's.)
import asyncio, json, os, re, ssl, subprocess, hashlib, yaml, websockets
try:
    from defusedxml.minidom import parseString as xml_parse
except ImportError:
    from xml.dom.minidom import parseString as xml_parse
# Defaults are THIS homelab's real endpoints. The old placeholders ("homeassistant.local",
# "user@...") resolved nowhere, so a first run always died halfway — after writing the SVG but
# before saving the view. Both stay env-overridable.
URI=os.environ.get("HA_WS_URI","wss://ha.jphe.in/api/websocket"); TOKEN=os.environ["HA_TOKEN"]
# The Control Room lives on the dashboard titled "smol" (url_path `smol-mesh`) — that is the one
# JP opens. It was hardcoded to "dashboard-dashboard" (the general "Dashboard"), so every run
# rebuilt a COPY nobody looks at while the real one silently went stale: a #303 control added on
# 2026-07-27 appeared to save fine and was invisible for an hour. Verify with
# `lovelace/dashboards/list` before changing this. Overridable for testing.
DASH=os.environ.get("HA_DASH","smol-mesh")
SSLCTX=ssl.create_default_context()  # verifies by default
if os.environ.get("HA_WS_INSECURE"):  # explicit opt-out for a LAN self-signed HA cert (like curl -k)
    SSLCTX.check_hostname=False; SSLCTX.verify_mode=ssl.CERT_NONE
HA=os.environ.get("HA_SSH","jp@10.0.6.108"); WWW="/config/www/luna-cards"; LOCAL="/local/luna-cards"
# HARDWARE traits only — facts that are NOT derivable from anything the firmware publishes.
# Everything identifying (sigil name, running build, which nodes exist) now comes from the HA
# device registry, which the firmware itself authors; see discover_fleet(). The old KNOWN table
# hardcoded names per id and had ALREADY drifted: it called id5 "Silent Aegis", but "Silent" is
# not in the fantasy adjective corpus at all (names.rs) — id5 is Spectral Aegis. A second copy
# of a deterministic algorithm always loses to the original, so we stopped keeping one.
HW={5:{"headless":True}}   # id5 has no OLED (#headless) → telemetry-first box, no screen controls
ACCENT="var(--accent-color)"; PHOS="var(--primary-color)"; VT="'VT323','IBM Plex Mono',monospace"
NAJ="['unavailable','unknown','none','None','']"
def esc(s): return str(s).replace("&","&amp;").replace("<","&lt;").replace(">","&gt;")
def accent_top(c): return ("ha-card{position:relative;overflow:hidden}ha-card:before{content:'';position:absolute;top:0;left:0;right:0;height:2px;"
                           "background:linear-gradient(90deg,transparent,%s,transparent);opacity:.55}"%c)

# ---------- #40 leaf-mesh-OTA relay PROGRESS: phosphor fill-bar + phase chip (conditional) ----------
# ph=sensor.smol_<id>_ota_diag → [label, mushroom-color, fill-hex, mdi]. Default = relaying/pending.
PMAP=("{'confirmed':['✓ confirmed','green','#5bff9a','mdi:check-decagram'],"
      "'leaf-timeout':['⧗ leaf-timeout','amber','#ffc24b','mdi:timer-sand-complete'],"
      "'relay-failed':['✗ relay-failed','red','#ff6b6b','mdi:close-octagon'],"
      "'fetch-failed':['✗ fetch-failed','red','#ff6b6b','mdi:cloud-alert'],"
      "'mac-unknown':['? mac-unknown','red','#ff6b6b','mdi:help-rhombus'],"
      "'rolled-back':['↩ rolled-back','red','#ff6b6b','mdi:backup-restore']}")
PDEF="['↑ relaying','blue','#5bd0ff','mdi:progress-upload']"
def ota_progress_card(nid, sigil):
    # Shown ONLY while a relay matters: staged build present & this node not yet on it, OR the phase
    # is non-clean (leaf-timeout / *-failed / …). display:none when idle so the box stays calm.
    # STATE = sensor.smol_<id>_ota_relaydiag (last_wb %, drives the fill); phase chip = ota_diag.
    I=str(nid); rd=f"sensor.smol_{nid}_ota_relaydiag"; ph=f"sensor.smol_{nid}_ota_diag"; bd=f"sensor.smol_{nid}_build"
    pre=("{% set na="+NAJ+" %}{% set p=states('"+rd+"') %}{% set pf=(p|float(0)) if p not in na else 0 %}"
         "{% set ph=states('"+ph+"') %}{% set pm="+PMAP+" %}{% set pl=pm.get(ph,"+PDEF+") %}"
         "{% set staged=states('sensor.smol_ota_staged') %}{% set build=states('"+bd+"') %}"
         "{% set act=(staged not in na and staged!='none' and staged!=build) or (ph not in na and ph!='confirmed') %}")
    primary=pre+"{{ (p ~ '%') if p not in na else '•••' }}"
    secondary=(pre+"{% set wt=state_attr('"+rd+"','wb_total') %}{% set wd=state_attr('"+rd+"','wb_done') %}"
               "{{ pl[0] }} · {% if wt %}{{ wd }}/{{ wt }} blk{% elif build not in na and staged not in na and staged!='none' %}run {{ build }}→{{ staged }}{% else %}awaiting relay{% endif %}")
    style=(pre+
        "ha-card{display:{% if act %}block{% else %}none{% endif %};border-radius:0;border-top:none;border-bottom:none;"
        "margin-top:-2px;border-left:3px solid {{ pl[2] }};position:relative;overflow:hidden;"
        "background:repeating-linear-gradient(0deg,transparent 0 2px,rgba(0,0,0,.30) 2px 3px),"
        "linear-gradient(90deg,{{ pl[2] }}2e 0,{{ pl[2] }}2e {{ pf }}%,#04120a {{ pf }}%,#04120a 100%);}"
        "ha-card:before{content:'';position:absolute;top:0;bottom:0;left:{{ pf }}%;width:2px;background:{{ pl[2] }};"
        "box-shadow:0 0 9px {{ pl[2] }},0 0 3px {{ pl[2] }};opacity:{% if pf>0 and pf<100 %}.95{% else %}0{% endif %};}"
        "ha-card:after{content:'◈ MESH-OTA · "+esc(sigil)+"';position:absolute;top:6px;right:11px;font-size:8.5px;letter-spacing:1.5px;"
        "color:{{ pl[2] }};opacity:.75;font-family:"+VT+";z-index:2;}")
    info=(pre+".primary{font-family:"+VT+";font-size:30px;line-height:.85;color:{{ pl[2] }};text-shadow:0 0 8px {{ pl[2] }}66}"
          ".secondary{font-size:10.5px;opacity:.85;letter-spacing:.3px}")
    return {"type":"custom:mushroom-template-card","primary":primary,"secondary":secondary,
            "icon":pre+"{{ pl[3] }}","icon_color":pre+"{{ pl[1] }}",
            "card_mod":{"style":{".":style,"mushroom-state-info$":info,"mushroom-shape-icon$":"--icon-symbol-size:22px"}}}

# ---------- #70/#74 device alarm: self-hides when clean, shows the rollback/abnormal-reset story ----------
# Shown ONLY when ota_outcome is bad (rolled-back / *-failed) OR reset_reason is abnormal (panic/wdt/
# brownout/glitch) — the "what just happened to this board" surface #70 exists for. display:none when clean.
def device_card(nid, sigil):
    I=str(nid); oo=f"sensor.smol_{nid}_ota_outcome"; rr=f"sensor.smol_{nid}_reset_reason"
    sl=f"sensor.smol_{nid}_boot_slot"; up=f"sensor.smol_{nid}_uptime"; hp=f"sensor.smol_{nid}_heap_free"
    pre=("{% set oo=states('"+oo+"') %}{% set rr=states('"+rr+"') %}"
         "{% set bad_o=oo in ['rolled-back','relay-failed','fetch-failed','mac-unknown'] %}"
         "{% set bad_r=rr in ['panic','wdt','brownout','glitch'] %}{% set act=bad_o or bad_r %}"
         "{% set col='#ff6b6b' if (oo=='rolled-back' or rr in ['panic','brownout']) else ('#ffc24b' if act else '#5bff9a') %}")
    primary=pre+"{% if oo=='rolled-back' %}↩ OTA rolled back{% elif bad_o %}✗ OTA {{ oo }}{% elif bad_r %}⚠ reset: {{ rr }}{% else %}device{% endif %}"
    secondary=pre+"slot {{ states('"+sl+"') }} · reset {{ rr }} · up {{ states('"+up+"') }}s · heap {{ states('"+hp+"') }}"
    style=(pre+"ha-card{display:{% if act %}block{% else %}none{% endif %};border-radius:0;border-top:none;border-bottom:none;"
           "margin-top:-2px;border-left:3px solid {{ col }};background:#0b0402;position:relative;overflow:hidden;}"
           "ha-card:after{content:'◈ DEVICE · "+esc(sigil)+"';position:absolute;top:6px;right:11px;font-size:8.5px;letter-spacing:1.5px;"
           "color:{{ col }};opacity:.7;font-family:"+VT+";z-index:2;}")
    info=(pre+".primary{font-family:"+VT+";font-size:17px;line-height:1;color:{{ col }}}.secondary{font-size:10px;opacity:.85}")
    return {"type":"custom:mushroom-template-card","primary":primary,"secondary":secondary,
            "icon":pre+"{% if oo=='rolled-back' or bad_r %}mdi:alert-octagram{% else %}mdi:chip{% endif %}",
            "icon_color":pre+"{{ 'red' if col=='#ff6b6b' else ('amber' if col=='#ffc24b' else 'green') }}",
            "card_mod":{"style":{".":style,"mushroom-state-info$":info,"mushroom-shape-icon$":"--icon-symbol-size:20px"}}}

# ---------- #55 plugin visibility: compact toggle chips (fill = shown in boot menu) ----------
PLUGS=[("clock","mdi:clock-outline"),("snake","mdi:snake"),("bench","mdi:test-tube"),("batt","mdi:battery"),
       ("grid","mdi:transmission-tower"),("wled","mdi:led-strip-variant"),("about","mdi:information-outline"),
       ("familiar","mdi:paw")]   # #57 bit-7 — Familiar screen toggle (all-on now composes 00FF)
def plugin_chips(nid, present):
    chips=[{"type":"template","content":"plugins","icon":"mdi:puzzle-outline","icon_color":"grey"}]  # label chip
    for name,icon in PLUGS:
        e=f"input_boolean.smol_{nid}_plugin_{name}"
        if e not in present: continue
        chips.append({"type":"template","entity":e,"icon":icon,
            "icon_color":"{{ 'green' if is_state('"+e+"','on') else 'disabled' }}",
            "tap_action":{"action":"toggle"}})
    if len(chips)<=1: return None
    return {"type":"custom:mushroom-chips-card","alignment":"start","chips":chips,
            "card_mod":{"style":"ha-card{border-radius:0;border-top:none;border-bottom:none;margin-top:-1px;padding:6px 8px 4px;"
                        "background:var(--card-background-color);}"}}

# ---------- node box = mushroom header + mushroom OLED + entities; span-4 in the VIEW grid ----------
def live_expr(meta, present):
    """Jinja boolean for 'this node is on the air', id-shape-agnostic.
    ORs the package's hand-written `binary_sensor.smol_<id>_online` (where a human wrote one)
    with the firmware-discovered entities, which carry expire_after. Either alone is wrong: the
    package sensor exists for only 5/7/8/9 (so id50/51/122 would read a permanent ⛔), and id5's
    happens to sit at 'unavailable' while the board is demonstrably alive."""
    parts=[]
    ob=f"binary_sensor.smol_{meta['id']}_online"
    if ob in present: parts.append(f"is_state('{ob}','on')")
    for f in HEARTBEAT:
        e=meta.get("fw",{}).get(f)
        if e: parts.append(f"states('{e}') not in {NAJ}")
    return "("+" or ".join(parts)+")" if parts else "false"

def node_card(nid, meta, present, span=4):
    gate=meta["gate"]; headless=meta.get("headless",False); I=str(nid); on=meta["onx"]
    # #64: the gateway's WiFi-uplink RSSI entity — RESOLVED from the registry, not guessed.
    # HA stickiness gave the fleet two forms (`sensor.smol_7_dominion_uplink` vs
    # `sensor.smol_50_ember_uplink`), so the old name-derived f-string missed most nodes.
    up=meta["fw"].get("uplink") or f"sensor.smol_{nid}_uplink"
    E_T=meta["fw"].get("temp") or f"sensor.smol_{nid}_temp"
    E_V=meta["fw"].get("voltage") or f"sensor.smol_{nid}_voltage"
    # RSSI pip LIVE (re-evaluates on takeover): gateway → its WiFi-uplink dBm (#64, falls to
    # 'WiFi' until the first burst publishes it); leaf → mesh-bond dBm.
    rssi_pip=(" · {% if gw %}{% set u=states('"+up+"') %}{% if u not in na %}{{ u }} dBm ↑{% else %}WiFi{% endif %}"
              "{% else %}{{ states('sensor.smol_"+I+"_rssi') if states('sensor.smol_"+I+"_rssi') not in na else '—' }} dBm{% endif %}")
    # LIVE gateway signal = the MESH-WIDE elected owner, not a per-id peers entity. The old
    # `sensor.smol_<id>_peers` form only exists for 7/8/9, so when the crown moved to id50 no
    # node could report itself as gateway at all. `smol/mesh/channel` is one fixed topic every
    # crown publishes to, so this works for any id — including ones HA has never seen before.
    gw="(state_attr('sensor.smol_mesh_channel','owner')|string == '"+I+"')"
    hdr=("{% set on="+on+" %}{% set gw="+gw+" %}{% set t=states('"+E_T+"') %}{% set v=states('"+E_V+"') %}"
         "{% set na="+NAJ+" %}{% if not on %}⛔ OFFLINE{% elif gw %}👑 GATEWAY{% else %}◈ leaf{% endif %}"
         " · {{ t if t not in na else '—' }}° · {{ v if v not in na else '—' }}V"+rssi_pip+" · id"+I)
    header={"type":"custom:mushroom-template-card","primary":meta["name"],"secondary":hdr,
            "icon":"{% if "+gw+" %}mdi:crown{% else %}mdi:chip{% endif %}",
            "icon_color":"{% if "+gw+" %}amber{% elif "+on+" %}green{% else %}red{% endif %}",
            "badge_icon":"{% if "+gw+" %}mdi:crown{% elif "+on+" %}mdi:leaf-circle{% else %}mdi:lan-disconnect{% endif %}",
            "badge_color":"{% if "+gw+" %}amber{% elif "+on+" %}green{% else %}red{% endif %}",
            "card_mod":{"style":{
                ".":("ha-card{border-radius:10px 10px 0 0;border-bottom:none;position:relative;overflow:hidden;"
                     "border:2px solid {% if "+gw+" %}var(--accent-color){% elif "+on+" %}var(--ha-card-border-color){% else %}#ff6b6b{% endif %};"
                     "opacity:{% if "+on+" %}1{% else %}.6{% endif %};"
                     "box-shadow:{% if "+gw+" %}0 0 18px -3px var(--accent-color){% else %}none{% endif %}}"
                     "ha-card:before{content:'';position:absolute;top:0;left:0;right:0;height:2px;background:linear-gradient(90deg,transparent,{% if "+gw+" %}var(--accent-color){% else %}var(--primary-color){% endif %},transparent);opacity:.6}"),
                "mushroom-state-info$":".primary{font-family:"+VT+";font-size:26px;line-height:.9}.secondary{font-size:11px}"}}}
    # mini-OLED shows the SCREEN's content (like the board): Grid→grid W, Batt→HV SOC, Clock→time, else temp.
    # Prefers the LIVE actual screen (sensor._screen, incl. manual nav) once #50 ships; falls to commanded while unknown.
    scr="(states('sensor.smol_"+I+"_screen') if states('sensor.smol_"+I+"_screen') not in "+NAJ+" else states('input_select.smol_"+I+"_screen'))"
    oled_p=("{% set scr="+scr+" %}{% set t=states('sensor.smol_"+I+"_temp') %}{% set g=states('sensor.smol_display_grid') %}{% set na="+NAJ+" %}"
            "{% if not "+on+" %}—{% elif scr=='Grid' %}{{ g.split('|')[1] if '|' in g else '—' }}"
            "{% elif scr=='Batt' %}{{ states('sensor.ev_battery_soc') }}%{% elif scr=='Clock' %}{{ now().strftime('%H:%M') }}"
            "{% elif scr=='Custom' %}{{ states('sensor.smol_"+I+"_custom')[:8] if states('sensor.smol_"+I+"_custom') not in na else '—' }}"  # #45
            "{% else %}{{ t if t not in na else '—' }}{% endif %}")
    oled_s=("{% set scr="+scr+" %}{{ scr|upper }} · {% if not "+on+" %}no link{% elif scr=='Grid' %}shared glass{% elif scr=='Batt' %}HV pack{% elif scr=='Clock' %}mesh time{% elif scr=='Custom' %}user lines{% else %}live °F{% endif %}")
    oled={"type":"custom:mushroom-template-card","primary":oled_p,"secondary":oled_s,"icon":"mdi:blank",
          "card_mod":{"style":{".":("ha-card{background:#020402;border:1px solid var(--ha-card-border-color);border-radius:0;"
                "box-shadow:inset 0 0 12px rgba(0,0,0,.9);position:relative;overflow:hidden;margin-top:-2px;opacity:{% if "+on+" %}1{% else %}.6{% endif %}}mushroom-shape-icon{display:none}"),
                "mushroom-state-info$":(".primary{font-family:"+VT+";font-size:44px;line-height:.8;color:var(--primary-color);"
                "text-shadow:0 0 7px rgba(91,255,154,.55)}.secondary{color:var(--primary-color);opacity:.7;font-size:10px}")}}}
    OP="opacity:{% if "+on+" %}1{% else %}.6{% endif %}"
    def prow(lst,eid,nm,icon=None):
        if eid in present:
            r={"entity":eid,"name":nm}
            if icon: r["icon"]=icon
            lst.append(r)
    # ---- ctrl_top: screen & mode + readback-always (config/screen/activity) ----
    top=[{"type":"section","label":"screen & mode"}]
    prow(top,f"input_select.smol_{nid}_screen","default screen")
    prow(top,f"input_select.smol_{nid}_page","page")
    prow(top,f"input_select.smol_{nid}_led","LED (status / on / off)","mdi:led-on")            # #48
    prow(top,f"input_text.smol_{nid}_custom","Custom lines (‹sa› text, | per line)","mdi:card-text")  # #45 · edit when screen=Custom
    prow(top,f"input_text.smol_{nid}_tale","Story opening (Bard)","mdi:feather")               # #303 · empty = this node's own protagonist
    prow(top,f"sensor.smol_{nid}_tale","  ↳ in use","mdi:book-open-variant")                   # #303 readback of the retained prompt
    prow(top,f"input_number.smol_{nid}_bard_speed","Typewriter (ms/char)","mdi:speedometer")    # #302 reveal clock, NOT the generation clock
    prow(top,f"input_select.smol_{nid}_bard_mode","Delivery (inf / page)","mdi:book-open-page-variant")  # #302 endless vs one screenful
    prow(top,f"input_select.smol_{nid}_bard_font","Text size","mdi:format-size")                # #302 bigger text = fewer chars on the glass
    prow(top,f"sensor.smol_{nid}_delivery","  ↳ in use","mdi:play-speed")                       # #302 readback of the retained <ms>:<mode>
    prow(top,f"input_button.smol_{nid}_apply",f"Apply → id{nid}","mdi:send")
    prow(top,f"input_button.smol_{nid}_reset","Reset to board default","mdi:backup-restore")
    rb=f"input_button.smol_{nid}_reboot"                                                       # #52 tap-guarded reboot
    if rb in present:
        top.append({"entity":rb,"name":"Reboot node","icon":"mdi:restart-alert",
            "tap_action":{"action":"perform-action","perform_action":"input_button.press","target":{"entity_id":rb},
                          "confirmation":{"text":f"Reboot id{nid} ({meta['name']})? It drops off the mesh briefly."}}})
    top.append({"type":"section","label":"readback"})
    prow(top,f"sensor.smol_{nid}_config","default screen (commanded)","mdi:monitor-dashboard") # commanded (works now)
    prow(top,f"sensor.smol_{nid}_screen","current screen (live)","mdi:monitor-eye")            # actual incl. manual nav; 'unknown' until #50
    prow(top,f"sensor.smol_{nid}_status","activity","mdi:pulse")
    ctrl_top={"type":"entities","show_header_toggle":False,"entities":top,
              "card_mod":{"style":"ha-card{border-radius:0;border-top:none;border-bottom:none;margin-top:-2px;"+OP+"}"}}
    # ---- LIVE role-conditional groups: box RESTRUCTURES on #51 takeover (keyed to owner attr).
    #      Rows added unconditionally; the hidden conditional also hides its (role-absent) entities → no 'entity not found'. ----
    JOIN="ha-card{border-radius:0;border-top:none;border-bottom:none;margin-top:-1px;"+OP+"}"
    # Role conditions read the MESH-WIDE elected owner attribute rather than a per-id peers
    # entity, for the same reason `gw` above does: the per-id form doesn't exist for most of
    # the fleet. HA's conditional card supports `attribute` in the modern condition form.
    OWNER="sensor.smol_mesh_channel"
    leaf_rows=[{"type":"section","label":"mesh bond (leaf)"}]
    prow(leaf_rows,f"sensor.smol_{nid}_rssi","bond (RSSI)","mdi:signal")
    prow(leaf_rows,f"sensor.smol_{nid}_rssi_band","bond band","mdi:signal-cellular-2")
    prow(leaf_rows,f"binary_sensor.smol_{nid}_resync","re-syncing","mdi:sync")
    cond_leaf={"type":"conditional",                                                           # shown when this node is NOT the elected crown
        "conditions":[{"condition":"state","entity_id":OWNER,"attribute":"owner","state_not":I}],
        "card":{"type":"entities","show_header_toggle":False,"card_mod":{"style":JOIN},"entities":leaf_rows}}
    gw_rows=[{"type":"section","label":"gateway anchor · WiFi uplink"},
             {"entity":up,"name":"WiFi uplink (RSSI)","icon":"mdi:wifi-arrow-up"},  # #64, registry-resolved
             {"type":"attribute","entity":OWNER,"attribute":"channel","name":"mesh channel (owned)","icon":"mdi:wifi"},
             {"type":"attribute","entity":OWNER,"attribute":"seq","name":"mesh seq (advancing)","icon":"mdi:counter"}]
    prow(gw_rows,f"sensor.smol_{nid}_peers","peers / roster","mdi:lan")
    cond_gw={"type":"conditional",                                                             # shown when this node IS the elected crown
        "conditions":[{"condition":"state","entity_id":OWNER,"attribute":"owner","state":I}],
        "card":{"type":"entities","show_header_toggle":False,"card_mod":{"style":JOIN},"entities":gw_rows}}
    # ---- ctrl_bottom: firmware + install (always last → rounded bottom) ----
    bot=[{"type":"section","label":"firmware"}]
    # #40 changed the Update discovery object_id noun-based → nounless (smol_<id>_update, wifi.rs 5efee40),
    # so match BOTH the legacy noun form (update.smol_<id>_<noun>_update, kept by HA registry stickiness on
    # id7/8/9) AND the new nounless form a fresh node (id10+) / a registry reset now creates.
    fw=meta["fw"].get("update")
    if fw: bot.append({"entity":fw,"name":"firmware (version + update)"})
    inst=f"input_button.smol_ota_install_{nid}"
    if inst in present: bot.append({"entity":inst,"name":"Install staged (gateway consumes)","icon":"mdi:rocket-launch"})
    ctrl_bottom={"type":"entities","show_header_toggle":False,"entities":bot,
                 "card_mod":{"style":"ha-card{border-radius:0 0 10px 10px;border-top:none;margin-top:-1px;"+OP+"}"}}
    # ---- #vitals ALWAYS-ON telemetry (heap / uptime / boot / slot / reset + mesh counters).
    #      These rich diag sensors existed but only surfaced in the fault-only device_card
    #      (self-hides when clean) → invisible in normal operation (JP: "not dynamic enough
    #      with data"). Now live on every box; prow() skips whatever a given node lacks. ----
    vit=[{"type":"section","label":"telemetry · live"}]
    prow(vit,f"sensor.smol_{nid}_heap_free","heap free","mdi:memory")
    prow(vit,f"sensor.smol_{nid}_heap_min","heap min (worst)","mdi:memory-arrow-down")
    prow(vit,f"sensor.smol_{nid}_uptime","uptime","mdi:timer-outline")
    prow(vit,f"sensor.smol_{nid}_boot_count","boot count","mdi:counter")
    prow(vit,f"sensor.smol_{nid}_boot_slot","OTA slot (running)","mdi:swap-horizontal-bold")
    prow(vit,f"sensor.smol_{nid}_reset_reason","last reset","mdi:restart-alert")
    for e,nm,ic in [("mesh_rx","mesh rx","mdi:arrow-down-bold-box"),("mesh_tx","mesh tx","mdi:arrow-up-bold-box"),
                    ("mesh_loss","mesh loss","mdi:alert-circle-outline")]:
        prow(vit,f"sensor.smol_{nid}_{e}",nm,ic)
    telemetry={"type":"entities","show_header_toggle":False,"entities":vit,"card_mod":{"style":JOIN}}
    # per-node heap history (24h) — the live trend, seamless in the stack (#dynamic-data + history).
    heap_hist=None
    if f"sensor.smol_{nid}_heap_free" in present:
        heap_hist={"type":"history-graph","hours_to_show":24,
                   "entities":[{"entity":f"sensor.smol_{nid}_heap_free","name":"heap free"}]
                              +([{"entity":f"sensor.smol_{nid}_heap_min","name":"heap min"}] if f"sensor.smol_{nid}_heap_min" in present else []),
                   "card_mod":{"style":"ha-card{border-radius:0;border-top:none;border-bottom:none;margin-top:-1px;"+OP+"}"}}
    ota=ota_progress_card(nid,meta['name'])   # #40 relay progress bar + phase chip — self-hides (display:none) when idle
    dev=device_card(nid,meta['name'])         # #70/#74 rollback / abnormal-reset alarm — self-hides when clean
    plug=plugin_chips(nid,present)  # #55 plugin-visibility toggle chips
    if headless:
        # #headless board: no OLED / screen-mode / plugin controls to show — telemetry-first box.
        seq=[header,telemetry]+([heap_hist] if heap_hist else [])+[ota,dev,ctrl_bottom]
    else:
        seq=[header,oled,ctrl_top,cond_leaf,cond_gw,telemetry]+([heap_hist] if heap_hist else [])+[ota,dev]
        if plug: seq.append(plug)
        seq.append(ctrl_bottom)
    return {"type":"vertical-stack","view_layout":{"grid-column":f"span {span}"},"cards":seq}

def legend_card(nodes, present):
    ents=[{"type":"section","label":"the mesh"}]
    if "sensor.smol_mesh_channel" in present:
        for a,nm,ic in [("owner","the Seat (owner id)","mdi:crown"),("channel","elected channel","mdi:wifi"),("seq","mesh seq","mdi:counter")]:
            ents.append({"type":"attribute","entity":"sensor.smol_mesh_channel","attribute":a,"name":nm,"icon":ic})
    for e,nm,ic in [("binary_sensor.smol_mesh_reelecting","re-electing","mdi:crown-outline"),("binary_sensor.smol_mesh_asleep","mesh asleep","mdi:sleep")]:
        if e in present: ents.append({"entity":e,"name":nm,"icon":ic})
    ents.append({"type":"section","label":"sigils & bonds (bond=RSSI · adrift when ch≠mesh)"})
    for n in nodes:
        # Prefer the package's online sensor; fall back to a firmware-discovered entity so a
        # node with no hand-written family still gets a row instead of 'entity not found'.
        row=f"binary_sensor.smol_{n['id']}_online"
        if row not in present: row=n["fw"].get("status") or n["fw"].get("temp")
        if row: ents.append({"entity":row,"name":f"{'♛ ' if n['gate'] else ''}{n['name']} · id{n['id']}"})
        if f"sensor.smol_{n['id']}_rssi" in present: ents.append({"entity":f"sensor.smol_{n['id']}_rssi","name":"   ↳ bond (RSSI)","icon":"mdi:signal"})
        if f"sensor.smol_{n['id']}_peers" in present: ents.append({"entity":f"sensor.smol_{n['id']}_peers","name":"   ↳ peers","icon":"mdi:lan"})
    return {"type":"entities","title":"the mesh","show_header_toggle":False,"entities":ents,
            "card_mod":{"style":accent_top(PHOS)},"view_layout":{"grid-column":"span 5"}}

# ---------- dormant sigils: registered with HA, not currently on the air ----------
# These are boards the firmware once announced whose firmware-discovered entities have all
# expired. Deliberately NOT hidden — a node that quietly disappears is how ids 7 and 9 stayed
# on the dashboard for weeks after their hardware was re-flashed as 50/51. One compact card,
# so a re-flashed ghost is visible and prunable without eating a full box.
def dormant_card(dormant):
    lines=["**dormant sigils** · registered with HA, silent on the air",
           "_a sigil that never returns is a stale identity — clear its MQTT device in HA to retire it_",""]
    for n in dormant:
        sw=n.get("sw") or "—"
        lines.append(f"· **{esc(n['name'])}** — last seen running `{esc(sw)}` · <small>id{n['id']}</small>")
    return {"type":"markdown","content":"\n".join(lines),"view_layout":{"grid-column":"span 12"},
            "card_mod":{"style":"ha-card{opacity:.72;"+accent_top("#6f8f78")+"}"}}

# ---------- FLEET-OTA row: staged build vs each node's RUNNING build# (+ live relay % / phase) ----------
# Uses sensor.smol_<id>_build (running, #40) vs sensor.smol_ota_staged; when a node lags the staged
# build it shows run **B** → **S** with the live relay % + phase chip. Scales with the fleet.
def forge_ota_md(nodes, present):
    out=["**fleet · staged vs running**",
         "staged **{% set s=states('sensor.smol_ota_staged') %}{{ s if s not in "+NAJ+" else '— none' }}**"]
    for n in nodes:
        I=str(n["id"]); tag=" ⚑" if n["gate"] else ""
        row=("{% set na="+NAJ+" %}{% set s=states('sensor.smol_ota_staged') %}{% set b=states('sensor.smol_"+I+"_build') %}"
             "{% set ph=states('sensor.smol_"+I+"_ota_diag') %}{% set p=states('sensor.smol_"+I+"_ota_relaydiag') %}"
             "{% set on="+n["onx"]+" %}{% set pm="+PMAP+" %}{% set pl=pm.get(ph,"+PDEF+") %}"
             "**"+esc(n["name"])+"**"+tag+" <small>id"+I+"</small> — "
             "{% if not on %}·offline·"
             "{% elif s in na or s=='none' %}run **{{ b }}** · no image staged"
             "{% elif b==s %}✓ **{{ b }}** current"
             "{% else %}run **{{ b }}** → **{{ s }}** · {% if p not in na %}**{{ p }}%** {% endif %}{{ pl[0] if ph not in na and ph!='none' else '↑ pending' }}"
             "{% endif %}")
        out.append(row)
    return "\n\n".join(out)

# ---------- per-node install buttons, canary/gateway-first (replaces the FORGE_INSTALL marker) ----------
def forge_install_rows(nodes, present):
    rows=[{"type":"section","label":"install staged → node · canary-first, one at a time"}]
    for n in nodes:   # nodes are pre-sorted gateway-first
        I=str(n["id"]); inst=f"input_button.smol_ota_install_{I}"
        if inst in present:
            rows.append({"entity":inst,"name":f"Install → {n['name']} · id{I}"+(" · canary / the Seat" if n["gate"] else ""),
                         "icon":"mdi:rocket-launch" if n["gate"] else "mdi:rocket-launch-outline"})
    return rows

# ---------- mesh-overview matrix — crown/channel + per-node build / slot / reset / OTA state ----------
# The "whole fleet at one glance" table JP asked for. Every cell is templated so it re-evaluates
# live (crown moves on #51 takeover; build flips as OTA converges). Build shows run →staged when behind.
def mesh_overview_md(nodes, dormant, present):
    na=NAJ
    # The Seat is named by its SIGIL; the id is the fine print. `owner` is the live mesh-wide
    # attribute, so the crown label follows a #51 takeover without a rebuild — but the NAME has
    # to come from the build-time fleet map, so we template a tiny id→sigil lookup inline.
    sig={str(n["id"]):n["name"] for n in list(nodes)+list(dormant)}
    head=("{% set na="+na+" %}{% set sig="+json.dumps(sig)+" %}"
          "{% set owner=state_attr('sensor.smol_mesh_channel','owner')|string %}"
          "{% set ch=states('sensor.smol_mesh_channel') %}{% set seq=state_attr('sensor.smol_mesh_channel','seq') %}"
          "{% set re=is_state('binary_sensor.smol_mesh_reelecting','on') %}"
          "**the mesh** · {% if re %}⚠️ **RE-ELECTING** — choosing a gateway…{% else %}"
          "👑 the Seat **{{ sig.get(owner, 'unknown sigil') }}** <small>id{{ owner if owner not in na else '?' }}</small> · "
          "ch **{{ ch if ch not in na else '—' }}** · "
          "seq **{{ seq if seq not in na else '—' }}** _(advancing = alive)_{% endif %}")
    rows=["", "| sigil | link | build | slot | last reset | OTA |", "|:--|:--:|:--:|:--:|:--:|:--:|"]
    for n in nodes:
        I=str(n["id"]); tag="♛ " if n["gate"] else ""
        rows.append(
            "{% set on="+n["onx"]+" %}"
            "{% set b=states('sensor.smol_"+I+"_build') %}{% set sl=states('sensor.smol_"+I+"_boot_slot') %}"
            "{% set rr=states('sensor.smol_"+I+"_reset_reason') %}{% set oo=states('sensor.smol_"+I+"_ota_outcome') %}"
            "{% set st=states('sensor.smol_ota_staged') %}"
            "| "+tag+"**"+esc(n["name"])+"** <small>id"+I+"</small> "
            "| {{ '🟢' if on else '⛔' }} "
            "| {{ b if b not in na else '—' }}{% if b not in na and st not in na and st!='none' and b!=st %} →{{ st }}{% endif %} "
            "| {{ sl|replace('ota_','slot ') if sl not in na else '—' }} "
            "| {{ rr if rr not in na else '—' }} "
            "| {{ oo if oo not in na else '—' }} |")
    # Dormant sigils get a row too — the whole point of dynamic discovery is that the table
    # matches the registry, so a re-flashed ghost is visible rather than quietly absent.
    for n in dormant:
        rows.append(f"| _{esc(n['name'])}_ <small>id{n['id']}</small> | ⚫ | _{esc(n.get('sw') or '—')}_ | — | — | _dormant_ |")
    return head+"\n"+"\n".join(rows)

# ---------- vitals history — heap-free + uptime across the WHOLE fleet, 24h (JP: history graphs) ----------
def vitals_history_cards(nodes, present):
    def graph(field, title, accent):
        ents=[{"entity":f"sensor.smol_{n['id']}_{field}","name":n["name"]}
              for n in nodes if f"sensor.smol_{n['id']}_{field}" in present]
        if not ents: return None
        return {"type":"history-graph","title":title,"hours_to_show":24,"entities":ents,
                "view_layout":{"grid-column":"span 6"},"card_mod":{"style":accent_top(accent)}}
    return [c for c in (graph("heap_free","heap free · 24h · all sigils",PHOS),
                        graph("uptime","uptime · 24h · reboots = sawtooth",ACCENT)) if c]

def gen_topology(nodes, seat, rost=None):
    """Sigil-labelled star of the CURRENT mesh. Edges are drawn from the crown's real retained
    roster when we have it (`rost['peers']`), so a leaf the crown cannot actually hear renders
    as a dashed 'adrift' spur instead of a confident solid bond."""
    peers=(rost or {}).get("peers",{})
    W,H=680,300; cx,cy=W/2,H*0.40; F="ui-monospace,'DejaVu Sans Mono',monospace"
    P=[f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}">',
       '<defs><pattern id="dg" width="16" height="16" patternUnits="userSpaceOnUse"><circle cx="1.5" cy="1.5" r=".8" fill="#0f3a24"/></pattern>'
       '<radialGradient id="sg" cx="50%" cy="50%" r="50%"><stop offset="0%" stop-color="#5bff9a" stop-opacity=".5"/><stop offset="100%" stop-color="#5bff9a" stop-opacity="0"/></radialGradient></defs>',
       f'<rect width="{W}" height="{H}" fill="#020402"/><rect width="{W}" height="{H}" fill="url(#dg)"/>',
       f'<text x="{W-14}" y="22" text-anchor="end" font-family="{F}" font-size="12" fill="#2f7a4e">SHARED MESH</text>']
    leaves=[n for n in nodes if n["id"]!=seat["id"]]; m=len(leaves); ly=H*0.80
    for i,lf in enumerate(leaves):
        lx=W*0.16+(W*0.68)*(i/(m-1) if m>1 else .5); on=lf["on"]; col="#5bff9a" if on else "#ff6b6b"
        bond=peers.get(lf["id"])                       # the crown's own view of this leaf, if any
        anim='<animate attributeName="opacity" values="0.9;0.5;0.9" dur="3s" repeatCount="indefinite"/>' if on else ''
        # solid = the crown lists this leaf in its roster; dashed = we cannot prove the bond
        bonded=bond is not None
        P.append(f'<line x1="{cx:.0f}" y1="{cy:.0f}" x2="{lx:.0f}" y2="{ly:.0f}" stroke="{col}" stroke-width="{3 if bonded else 1.5}"{"" if bonded else " stroke-dasharray=\"6 5\""} opacity="{.85 if on else .7}">{anim}</line>')
        if bond:
            P.append(f'<text x="{(cx+lx)/2:.0f}" y="{(cy+ly)/2:.0f}" text-anchor="middle" font-family="{F}" font-size="10" fill="#2f7a4e">{bond["rssi"]} dBm</text>')
        P.append(f'<circle cx="{lx:.0f}" cy="{ly:.0f}" r="11" fill="#020402" stroke="{col}" stroke-width="{2.5 if on else 2}"/>')
        P.append(f'<text x="{lx:.0f}" y="{ly+27:.0f}" text-anchor="middle" font-family="{F}" font-size="16" font-weight="600" fill="{"#c9e8d2" if on else "#6f8f78"}">{esc(lf["name"])}</text>')
        state="attuned" if bonded else ("no bond" if on else "offline")
        P.append(f'<text x="{lx:.0f}" y="{ly+43:.0f}" text-anchor="middle" font-family="{F}" font-size="11" fill="{col}">id{lf["id"]} · {state}</text>')
    P.append(f'<circle cx="{cx:.0f}" cy="{cy:.0f}" r="46" fill="url(#sg)"/>')
    P.append(f'<circle cx="{cx:.0f}" cy="{cy:.0f}" r="13" fill="#020402" stroke="#ffc24b" stroke-width="2.5"><animate attributeName="r" values="13;15.5;13" dur="2.6s" repeatCount="indefinite"/></circle>')
    P.append(f'<text x="{cx:.0f}" y="{cy+6:.0f}" text-anchor="middle" font-family="{F}" font-size="18" fill="#ffc24b">&#9819;</text>')
    # SIGIL FIRST: the name is the headline, the raw id is the fine print beneath it.
    P.append(f'<text x="{cx:.0f}" y="{cy-30:.0f}" text-anchor="middle" font-family="{F}" font-size="20" font-weight="600" fill="#c9e8d2">{esc(seat["name"])}</text>')
    ch=(rost or {}).get("ch")
    P.append(f'<text x="{cx:.0f}" y="{cy+34:.0f}" text-anchor="middle" font-family="{F}" font-size="12" fill="#ffc24b">the Seat · id{seat["id"]}{" · ch"+esc(ch) if ch else ""}</text>')
    P.append('</svg>')
    return "".join(P)

def serve(name, svg):
    xml_parse(svg); open(name,"w").write(svg)
    subprocess.run(["ssh",HA,f"sudo tee {WWW}/{name} >/dev/null"],input=svg.encode(),check=True)
    return f"{LOCAL}/{name}?v={hashlib.md5(svg.encode()).hexdigest()[:8]}"

async def rpc(ws,m,_i=[1]):
    m=dict(m); m["id"]=_i[0]; _i[0]+=1; await ws.send(json.dumps(m))
    while True:
        r=json.loads(await ws.recv())
        if r.get("id")==m["id"]: return r

# ============================ DYNAMIC FLEET DISCOVERY ==================================
# The fleet is whatever the FIRMWARE says it is. Each node publishes retained MQTT discovery
# configs (`homeassistant/sensor/smol<id>/{temp,voltage,status}/config`, plus uplink + the OTA
# update entity) carrying a `device` block:
#     {"identifiers":["smol<id>"], "name":"smol <id> <Noun>",
#      "model":"smol ESP32-C3",    "sw_version":"v<build> <ForgeNoun>"}
# so HA's device registry IS a firmware-authored fleet manifest — ids, sigil names and running
# builds, all self-reported. We READ the sigil from there; we never recompute it. `names.rs`'s
# formula already changed once (#218) and a duplicated algorithm drifts silently, so the only
# copy that matters is the one on the board.
#
# This replaced discovery-by-`binary_sensor.smol_<id>_online`, which only found nodes that a
# human had hand-written an entity family for in ha/packages/smol_mesh.yaml (5/7/8/9). That
# rendered ids 7 and 9 — hardware re-flashed as 50/51 in July, gone from the air — while ids
# 50, 51 and 122 sat on the mesh with no box at all.
SIGIL_MODEL="smol "          # device.model prefix; excludes the C6 watch (`smolwatch042`)
def sigil_of(dev_name, nid):
    """The sigil words the FIRMWARE published, from `device.name` = 'smol <id> <words…>'.
    Returns whatever follows the id, so when firmware starts sending the adjective too
    ('smol 50 Kindled Ember') this picks it up with no change here. Falls back to a plainly
    provisional label rather than inventing a name."""
    m=re.match(r"^smol\s+%d\s+(.+)$"%nid, (dev_name or "").strip())
    return m.group(1).strip() if m else f"unnamed id{nid}"

# Firmware-discovery fields → resolved from the registry, NEVER built by convention.
# HA registry stickiness left the fleet in two irreconcilable naming forms: id7/8/9 got the
# nounless `sensor.smol_7_temp` but the noun-infixed `sensor.smol_7_dominion_uplink`, while
# id5/13/42/50/51/122/236 got `sensor.smol_50_ember_temp`. An f-string is therefore wrong for
# most of the fleet — the entity id has to be looked up.
HEARTBEAT=("status","temp","voltage","uplink")   # carry expire_after → genuine liveness
def resolve_fw(nid, own):
    out={}
    for f in HEARTBEAT:
        pat=re.compile(rf"^sensor\.smol_{nid}(?:_[a-z0-9]+)?_{f}$")
        out[f]=next((e for e in sorted(own) if pat.match(e)), None)
    out["update"]=next((e for e in sorted(own) if re.match(rf"^update\.smol_{nid}(?:_[a-z0-9]+)?_update$",e)), None)
    return out

async def discover_fleet(ws, st):
    """{id: meta} for every smol board the firmware has ever announced to HA."""
    devs=(await rpc(ws,{"type":"config/device_registry/list"}))["result"]
    ents=(await rpc(ws,{"type":"config/entity_registry/list"}))["result"]
    fleet={}
    for d in devs:
        if not (d.get("model") or "").startswith(SIGIL_MODEL): continue
        for ident in d.get("identifiers") or []:
            tail=str(ident[-1]) if isinstance(ident,(list,tuple)) else str(ident)
            m=re.fullmatch(r"smol(\d{1,3})",tail)
            if not m: continue
            nid=int(m.group(1))
            fleet[d["id"]]={"id":nid,"name":sigil_of(d.get("name_by_user") or d.get("name"),nid),
                            "sw":d.get("sw_version"),"own":[]}
            break
    for e in ents:
        if e.get("device_id") in fleet: fleet[e["device_id"]]["own"].append(e["entity_id"])
    out={}
    for meta in fleet.values():
        nid=meta["id"]; meta["fw"]=resolve_fw(nid,meta["own"])
        # LIVENESS — only the firmware-discovered entities may be trusted. They carry
        # expire_after (300 telemetry / 120 uplink) so they genuinely go unavailable when a
        # board stops talking. The hand-written package sensors mostly do NOT: ids 7/8/9 each
        # hold 40+ non-unavailable entities that are pure stale-retained ghosts, so "has a
        # live entity" would report long-dead hardware as present.
        # Kept in lockstep with live_expr()'s Jinja form — the same inputs, so a node the
        # dashboard paints green is never one this script filed as dormant.
        # HEARTBEAT fields only. `update` is excluded on purpose: it mirrors the RETAINED
        # `smol/<id>/ota/state` with no expire_after, so a long-dead board still reports a
        # cheerful 'off'/'on' there forever.
        NA=("unavailable","unknown","none","None","",None)
        meta["on"]=(any(st.get(meta["fw"][f],{}).get("state") not in NA
                        for f in HEARTBEAT if meta["fw"].get(f))
                    or st.get(f"binary_sensor.smol_{nid}_online",{}).get("state")=="on")
        meta.update(HW.get(nid,{})); out[nid]=meta
    return out

async def read_roster(ws, timeout=8.0):
    """Retained `smol/+/peers` straight off the broker — the crown's real bond list.
    Read here rather than through an entity because the topic is per-crown-id: the mirror in
    smol_mesh.yaml exists only for 7/8/9, so when the crown moved 8→50 the roster vanished from
    HA entirely even though `smol/50/peers` was sitting on the broker. Wire format (mode.rs
    serialize_peers): `PEERS|<role>|<ch>|id,rssi,age,ch,flags;…`, flags bit0=connected
    bit1=has_mesh_time. Returns {crown_id: {"ch":n,"peers":{id:{rssi,age,flags}}}} for role G."""
    sub=await rpc(ws,{"type":"mqtt/subscribe","topic":"smol/+/peers"})
    if not sub.get("success"): print("  !! mqtt/subscribe unavailable — topology falls back to registry only"); return {}
    raw={}
    try:
        while True:
            r=json.loads(await asyncio.wait_for(ws.recv(),timeout=timeout))
            if r.get("type")=="event" and r.get("id")==sub["id"]:
                ev=r["event"]
                m=re.fullmatch(r"smol/(\d{1,3})/peers",ev.get("topic",""))
                if m: raw[int(m.group(1))]=ev.get("payload","")
    except asyncio.TimeoutError: pass
    out={}
    for cid,payload in raw.items():
        p=payload.split("|")
        if len(p)<4 or p[0]!="PEERS" or p[1]!="G": continue   # leaves / malformed → not a roster
        peers={}
        for tok in p[3].split(";"):
            f=tok.split(",")
            if len(f)<5: continue
            try: pid,rssi,age,flags=int(f[0]),int(f[1]),int(f[2]),int(f[4])
            except ValueError: continue
            # rssi arrives as an UNSIGNED byte (firmware feeds rx_control.rssi through an i32
            # without the i8 cast), so -45 dBm is on the wire as 211. Decode defensively; the
            # publisher is the real fix (reported to the firmware agent).
            if rssi>127: rssi-=256
            # A roster can list the same id twice (one entry per MAC) — keep the strongest.
            if pid not in peers or rssi>peers[pid]["rssi"]:
                peers[pid]={"rssi":rssi,"age":age,"flags":flags}
        out[cid]={"ch":p[2],"peers":peers}
    return out

async def main():
    view=yaml.safe_load(open("smol-control-scaffold.yaml"))
    async with websockets.connect(URI,max_size=16*1024*1024,ssl=SSLCTX) as ws:
        json.loads(await ws.recv()); await ws.send(json.dumps({"type":"auth","access_token":TOKEN})); await ws.recv()
        st={s["entity_id"]:s for s in (await rpc(ws,{"type":"get_states"}))["result"]}; present=set(st)
        fleet=await discover_fleet(ws,st)
        roster=await read_roster(ws)
        if not fleet: print("!! no smol devices in the registry — nothing to render"); return
        # The Seat is the ELECTED crown, from the mesh-wide (id-agnostic) `smol/mesh/channel`.
        owner=st.get("sensor.smol_mesh_channel",{}).get("attributes",{}).get("owner")
        try: seat_id=int(owner)
        except (TypeError,ValueError): seat_id=None
        if seat_id not in fleet:                      # stale/absent MC → believe the roster's role-G publisher
            seat_id=next((c for c in roster if c in fleet), None)
        live={i for i,m in fleet.items() if m["on"]}
        if seat_id in fleet: live.add(seat_id)        # the crown is on the mesh by definition
        if not live: live=set(fleet)                  # nothing live at all → show everything rather than a blank room
        if seat_id is None: seat_id=min(live)
        for i,m in fleet.items():
            m["gate"]=(i==seat_id); m["bond"]=roster.get(seat_id,{}).get("peers",{}).get(i)
            m["onx"]=live_expr(m,present)   # id-shape-agnostic Jinja liveness, reused by every card
        nodes=sorted((fleet[i] for i in live),key=lambda n:(not n["gate"],not n["on"],n["id"]))
        dormant=sorted((m for i,m in fleet.items() if i not in live),key=lambda n:n["id"])
        seat=fleet[seat_id]
        topo_url=serve("smol-topology.svg", gen_topology(nodes,seat,roster.get(seat_id,{})))
        # adaptive fleet tiling: 4 sigils → 2×2 (span 6), 3 → 3-up (span 4), 2 → span 6, 1 → full width.
        per_row={1:1,2:2,4:2}.get(len(nodes),3); NODE_SPAN=12//per_row
        node_cards=[node_card(n["id"],n,present,NODE_SPAN) for n in nodes]
        if dormant: node_cards.append(dormant_card(dormant))
        legend=legend_card(nodes,present)
        mesh_ovw=mesh_overview_md(nodes,dormant,present); vitals=vitals_history_cards(nodes,present)
        cards=view["cards"]; out=[]; done={"topo":0,"legend":0,"meshovw":0,"fleet":0,"vitals":0,"forge":0,"install":0}
        for c in cards:
            if c.get("type")=="picture" and c.get("image")=="TOPO": c["image"]=topo_url; done["topo"]+=1; out.append(c)
            elif c.get("type")=="markdown" and c.get("content")=="LEGEND":
                lc=dict(legend); lc["view_layout"]=c.get("view_layout") or lc.get("view_layout"); done["legend"]+=1; out.append(lc)
            elif c.get("type")=="markdown" and c.get("content")=="MESHOVW":
                mc=dict(c); mc["content"]=mesh_ovw; done["meshovw"]+=1; out.append(mc)
            elif c.get("type")=="markdown" and c.get("content")=="FLEET":
                out.extend(node_cards); done["fleet"]+=1
            elif c.get("type")=="markdown" and c.get("content")=="VITALS":
                out.extend(vitals); done["vitals"]+=1
            else: out.append(c)
        view["cards"]=out
        def fill_forge(cs):                                   # FORGE_OTA + FORGE_INSTALL nested in the forge vertical-stack
            for c in cs:
                if c.get("type")=="markdown" and c.get("content")=="FORGE_OTA": c["content"]=forge_ota_md(nodes,present); done["forge"]+=1
                if c.get("type")=="entities" and any(isinstance(e,dict) and e.get("entity")=="FORGE_INSTALL" for e in c.get("entities",[])):
                    c["entities"]=forge_install_rows(nodes,present); done["install"]+=1
                if isinstance(c,dict) and "cards" in c: fill_forge(c["cards"])
        fill_forge(view["cards"])
        assert all(done.values()), f"placeholders not all filled: {done}"
        cfg=(await rpc(ws,{"type":"lovelace/config","url_path":DASH}))["result"]
        json.dump(cfg,open("lovelace_PRESAVE_backup.json","w"),indent=1)
        # --- NON-DESTRUCTIVE SAVE (2026-07-27 incident) --------------------------------------
        # This line used to be `[...remove smol-control...] + [view]`, which did two harmful
        # things: it DROPPED any card added to the live view that the scaffold does not know
        # about, and it APPENDED the rebuilt view, moving the Control Room to the end of the
        # dashboard. Both fired on 2026-07-27: ten cards vanished (the herald section, the
        # per-node overrides/IO section, and NTP freshness — all added live, never back-ported
        # to the scaffold) and the view demoted itself to second. The live dashboard drifts
        # AHEAD of the generator; a generator that rebuilds from a stale scaffold must merge,
        # not replace.
        def _ident(c):
            """Stable identity for a card, so an UPDATED generator card is recognised as the
            same card (not preserved as a duplicate) while a genuinely unknown card is kept."""
            t = c.get("type", "?")
            lbl = c.get("title") or (c.get("content", "")[:45] if t == "markdown" else "")
            if not lbl:
                # A NODE BOX has no title, so identify it by node id — that way an UPDATED box
                # (e.g. gaining the #303 story-prompt rows) is recognised as the same card.
                # Detect one the way the verifier below does: vertical-stack at the node span.
                # EVERYTHING else falls back to a hash of the whole card, NOT a JSON prefix:
                # prefixes collide (two untitled vertical-stacks share their first 45 chars, and a
                # fleet-wide card that merely mentions `smol_8_` looked like the id8 box), and a
                # collision makes a genuinely-new live card look "known" — silently dropping it,
                # which is precisely the bug this merge exists to prevent.
                # Match a node box at ANY span, not just this run's NODE_SPAN. The span is
                # derived from the fleet SIZE (per_row), so discovering one more node silently
                # re-keyed every existing box — the previous run's boxes then looked "unknown"
                # and were preserved alongside the new ones, doubling the fleet on screen.
                is_node_box = (t == "vertical-stack"
                               and re.fullmatch(r"span \d+", str((c.get("view_layout") or {}).get("grid-column") or "")))
                # A node box references EXACTLY ONE node id. Keying off the first id found
                # instead collided with the fleet-wide forge stack, whose OTA table mentions
                # `sensor.smol_5_build` and so identified as the id5 box — two different cards
                # sharing one identity, which is precisely how a live card gets silently
                # dropped. Several distinct ids ⇒ fleet-wide card ⇒ fall through to the hash.
                seen_ids = set(re.findall(r"smol_(\d+)_", json.dumps(c))) if is_node_box else set()
                if len(seen_ids) == 1:
                    lbl = f"node{seen_ids.pop()}"
                elif t == "vertical-stack" and (c.get("cards") or [{}])[0].get("title"):
                    # A wrapper stack inherits its FIRST CHILD's title. The forge is exactly
                    # this shape: generator-filled (its OTA table lists a row per node) but
                    # untitled at the top level, so it fell through to a content hash — and a
                    # content hash of a card whose content tracks the fleet changes whenever the
                    # fleet does. Each fleet change therefore stranded the previous forge as an
                    # "unknown" card and preserved it forever: JP's dashboard already carries
                    # TWO forge stacks from earlier runs, and this run would have added a third.
                    # An inherited title is fleet-invariant, so all copies collapse to one
                    # identity and the duplicates finally retire.
                    lbl = str((c.get("cards") or [{}])[0]["title"])
                else:
                    # Hash the card with CACHE-BUSTING QUERY STRINGS STRIPPED. `serve()` appends
                    # `?v=<md5-of-svg>` to the topology image, so a card that is semantically the
                    # same becomes byte-different whenever the SVG changes — the previous run's
                    # copy then looks "unknown" and gets preserved, and the view grows by a card or
                    # two on EVERY run. Caught it accreting 30 -> 32 with two sha-identified strays.
                    norm = re.sub(r"\?v=[0-9a-f]+", "", json.dumps(c, sort_keys=True))
                    lbl = "sha:" + hashlib.sha1(norm.encode()).hexdigest()[:12]
            return f"{t}|{lbl[:45]}"

        prev = next((v for v in cfg["views"] if v.get("path") == "smol-control"), None)
        idx = cfg["views"].index(prev) if prev else len(cfg["views"])
        # A node box is GENERATOR-OWNED: this script is the only thing that creates one, so when
        # a node leaves the fleet its box must DIE. Without this the merge below — whose whole
        # job is to protect cards it does not recognise — would faithfully resurrect the box of
        # every retired node forever (ids 7 and 9 being exactly that case).
        GEN_OWNED = re.compile(r"^vertical-stack\|node\d+$")
        if prev:
            known = {_ident(c) for c in view["cards"]}
            retired = [c for c in prev.get("cards", []) if _ident(c) not in known and GEN_OWNED.match(_ident(c))]
            extras = [c for c in prev.get("cards", []) if _ident(c) not in known and not GEN_OWNED.match(_ident(c))]
            if retired:
                print(f"  RETIRED {len(retired)} node box(es) for nodes no longer in the fleet:",
                      [_ident(c) for c in retired])
            if extras:
                view["cards"] = view["cards"] + extras
                print(f"  PRESERVED {len(extras)} live card(s) the scaffold does not define:")
                for c in extras:
                    print(f"    · {_ident(c)}")
                print("    → back-port these into smol-control-scaffold.yaml, or they stay orphaned here.")
        cfg["views"] = [v for v in cfg["views"]
                        if v.get("title") != "smol Nodes" and v.get("path") != "smol-control"]
        cfg["views"].insert(idx, view)        # in place: never reorder the user's dashboard
        s=await rpc(ws,{"type":"lovelace/config/save","url_path":DASH,"config":cfg})
        if not s.get("success"): print("!! SAVE FAILED",s); return
        r2=(await rpc(ws,{"type":"lovelace/config","url_path":DASH}))["result"]
        vv=next(x for x in r2["views"] if x.get("path")=="smol-control")
        # Count NODE boxes specifically, via the same identity the merge uses. Matching on
        # "vertical-stack at span N" over-counted: glass/power/forge are also span-N stacks, so
        # a 3-node fleet cheerfully reported 6 node boxes.
        nb=[c for c in vv["cards"] if GEN_OWNED.match(_ident(c))]
        print(f"SAVE ok · dashboard '{DASH}' · the Seat = {seat['name']} (id{seat_id})"
              f" · node span {NODE_SPAN}")
        print("  fleet (HA device registry, firmware-authored):")
        for n in nodes:
            b=n.get("bond"); bond=f" · bond {b['rssi']} dBm (age {b['age']}s)" if b else ""
            print(f"    {'♛' if n['gate'] else '·'} {n['name']:<22} id{n['id']:<4} {'live':<5}"
                  f" sw={n.get('sw') or '—':<12}{bond}")
        for n in dormant:
            print(f"    ⚫ {n['name']:<22} id{n['id']:<4} {'dormant':<5} sw={n.get('sw') or '—'}")
        if roster:
            for cid,r in sorted(roster.items()):
                print(f"  roster · crown id{cid} ch{r['ch']} → {sorted(r['peers'])}")
        print("  node boxes spliced into view grid:",len(nb),"· done:",done)
        print("  each box:",[c.get("type") for c in nb[0]["cards"]] if nb else "NONE")
if __name__=="__main__":
    asyncio.run(main())

#!/usr/bin/env python3
# smol · Control Room builder — MINIMAL FIX: un-nest the fleet (the black hole).
# JP screenshot: node cards were MISSING because they lived in a nested custom:grid-layout
# card that renders EMPTY. Fix = splice node cards DIRECTLY into the view grid (span 4, like
# glass/power/forge which render). Node cards stay LIVE mushroom boxes (header + OLED + entities).
# If mushroom still doesn't render un-nested → it's mushroom, swap to SVG faceplate then.
#   HA_TOKEN=<your-ha-long-lived-token> python3 build_control_room.py
import asyncio, json, os, re, ssl, subprocess, hashlib, yaml, websockets
try:
    from defusedxml.minidom import parseString as xml_parse
except ImportError:
    from xml.dom.minidom import parseString as xml_parse
URI=os.environ.get("HA_WS_URI","wss://homeassistant.local:8123/api/websocket"); TOKEN=os.environ["HA_TOKEN"]; DASH="dashboard-dashboard"
SSLCTX=ssl.create_default_context()  # verifies by default
if os.environ.get("HA_WS_INSECURE"):  # explicit opt-out for a LAN self-signed HA cert (like curl -k)
    SSLCTX.check_hostname=False; SSLCTX.verify_mode=ssl.CERT_NONE
HA=os.environ.get("HA_SSH","user@homeassistant.local"); WWW="/config/www/luna-cards"; LOCAL="/local/luna-cards"
KNOWN={7:{"name":"Draconic Dominion","role":"the Seat","gate":True},
       8:{"name":"Eldritch Nexus","role":"leaf"},
       9:{"name":"Jade Herald","role":"leaf"}}
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
def ota_progress_card(nid):
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
        "ha-card:after{content:'◈ MESH-OTA · id"+I+"';position:absolute;top:6px;right:11px;font-size:8.5px;letter-spacing:1.5px;"
        "color:{{ pl[2] }};opacity:.75;font-family:"+VT+";z-index:2;}")
    info=(pre+".primary{font-family:"+VT+";font-size:30px;line-height:.85;color:{{ pl[2] }};text-shadow:0 0 8px {{ pl[2] }}66}"
          ".secondary{font-size:10.5px;opacity:.85;letter-spacing:.3px}")
    return {"type":"custom:mushroom-template-card","primary":primary,"secondary":secondary,
            "icon":pre+"{{ pl[3] }}","icon_color":pre+"{{ pl[1] }}",
            "card_mod":{"style":{".":style,"mushroom-state-info$":info,"mushroom-shape-icon$":"--icon-symbol-size:22px"}}}

# ---------- #70/#74 device alarm: self-hides when clean, shows the rollback/abnormal-reset story ----------
# Shown ONLY when ota_outcome is bad (rolled-back / *-failed) OR reset_reason is abnormal (panic/wdt/
# brownout/glitch) — the "what just happened to this board" surface #70 exists for. display:none when clean.
def device_card(nid):
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
           "ha-card:after{content:'◈ DEVICE · id"+I+"';position:absolute;top:6px;right:11px;font-size:8.5px;letter-spacing:1.5px;"
           "color:{{ col }};opacity:.7;font-family:"+VT+";z-index:2;}")
    info=(pre+".primary{font-family:"+VT+";font-size:17px;line-height:1;color:{{ col }}}.secondary{font-size:10px;opacity:.85}")
    return {"type":"custom:mushroom-template-card","primary":primary,"secondary":secondary,
            "icon":pre+"{% if oo=='rolled-back' or bad_r %}mdi:alert-octagram{% else %}mdi:chip{% endif %}",
            "icon_color":pre+"{{ 'red' if col=='#ff6b6b' else ('amber' if col=='#ffc24b' else 'green') }}",
            "card_mod":{"style":{".":style,"mushroom-state-info$":info,"mushroom-shape-icon$":"--icon-symbol-size:20px"}}}

# ---------- #55 plugin visibility: compact toggle chips (fill = shown in boot menu) ----------
PLUGS=[("clock","mdi:clock-outline"),("snake","mdi:snake"),("bench","mdi:test-tube"),("batt","mdi:battery"),
       ("grid","mdi:transmission-tower"),("wled","mdi:led-strip-variant"),("about","mdi:information-outline")]
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
def node_card(nid, meta, present):
    gate=meta["gate"]; I=str(nid); on=f"is_state('binary_sensor.smol_{I}_online','on')"
    # #64: the gateway's WiFi-uplink RSSI entity is device-name-derived (HA discovery names
    # it sensor.smol_<id>_<noun>_uplink, where <noun> is the fantasy noun = last name word).
    up=f"sensor.smol_{nid}_{meta['name'].split()[-1].lower()}_uplink"
    # RSSI pip LIVE (re-evaluates on takeover): gateway → its WiFi-uplink dBm (#64, falls to
    # 'WiFi' until the first burst publishes it); leaf → mesh-bond dBm.
    rssi_pip=(" · {% if gw %}{% set u=states('"+up+"') %}{% if u not in na %}{{ u }} dBm ↑{% else %}WiFi{% endif %}"
              "{% else %}{{ states('sensor.smol_"+I+"_rssi') if states('sensor.smol_"+I+"_rssi') not in na else '—' }} dBm{% endif %}")
    # LIVE gateway signal = peers-state 'gateway' (an entity STATE → works in mushroom templates AND legacy conditional-card conditions).
    gw="is_state('sensor.smol_"+I+"_peers','gateway')"
    hdr=("{% set on="+on+" %}{% set gw="+gw+" %}{% set t=states('sensor.smol_"+I+"_temp') %}{% set v=states('sensor.smol_"+I+"_voltage') %}"
         "{% set na="+NAJ+" %}{% if not on %}⛔ OFFLINE{% elif gw %}👑 GATEWAY{% else %}◈ leaf{% endif %} · id"+I+""
         " · {{ t if t not in na else '—' }}° · {{ v if v not in na else '—' }}V"+rssi_pip)
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
    cond_leaf={"type":"conditional",                                                           # shown when this node is NOT the gateway (leaf/offline) — legacy state condition (HA-supported)
        "conditions":[{"entity":f"sensor.smol_{nid}_peers","state_not":"gateway"}],
        "card":{"type":"entities","show_header_toggle":False,"card_mod":{"style":JOIN},"entities":[
            {"type":"section","label":"mesh bond (leaf)"},
            {"entity":f"sensor.smol_{nid}_rssi","name":"bond (RSSI)","icon":"mdi:signal"},
            {"entity":f"sensor.smol_{nid}_rssi_band","name":"bond band","icon":"mdi:signal-cellular-2"},
            {"entity":f"binary_sensor.smol_{nid}_resync","name":"re-syncing","icon":"mdi:sync"}]}}
    cond_gw={"type":"conditional",                                                             # shown when this node IS the gateway — legacy state condition (HA-supported)
        "conditions":[{"entity":f"sensor.smol_{nid}_peers","state":"gateway"}],
        "card":{"type":"entities","show_header_toggle":False,"card_mod":{"style":JOIN},"entities":[
            {"type":"section","label":"gateway anchor · WiFi uplink"},
            {"entity":up,"name":"WiFi uplink (RSSI)","icon":"mdi:wifi-arrow-up"},  # #64: gateway AP-uplink RSSI (sensor.smol_<id>_<noun>_uplink)
            {"type":"attribute","entity":"sensor.smol_mesh_channel","attribute":"channel","name":"mesh channel (owned)","icon":"mdi:wifi"},
            {"type":"attribute","entity":"sensor.smol_mesh_channel","attribute":"seq","name":"mesh seq (advancing)","icon":"mdi:counter"},
            {"entity":f"sensor.smol_{nid}_peers","name":"peers / roster","icon":"mdi:lan"}]}}
    # ---- ctrl_bottom: firmware + install (always last → rounded bottom) ----
    bot=[{"type":"section","label":"firmware"}]
    # #40 changed the Update discovery object_id noun-based → nounless (smol_<id>_update, wifi.rs 5efee40),
    # so match BOTH the legacy noun form (update.smol_<id>_<noun>_update, kept by HA registry stickiness on
    # id7/8/9) AND the new nounless form a fresh node (id10+) / a registry reset now creates.
    fw=next((e for e in present if re.match(rf"update\.smol_{nid}(_.*)?_update$",e)),None)
    if fw: bot.append({"entity":fw,"name":"firmware (version + update)"})
    inst=f"input_button.smol_ota_install_{nid}"
    if inst in present: bot.append({"entity":inst,"name":"Install staged (gateway consumes)","icon":"mdi:rocket-launch"})
    ctrl_bottom={"type":"entities","show_header_toggle":False,"entities":bot,
                 "card_mod":{"style":"ha-card{border-radius:0 0 10px 10px;border-top:none;margin-top:-1px;"+OP+"}"}}
    ota=ota_progress_card(nid)   # #40 relay progress bar + phase chip — self-hides (display:none) when idle
    dev=device_card(nid)         # #70/#74 rollback / abnormal-reset alarm — self-hides when clean
    plug=plugin_chips(nid,present)  # #55 plugin-visibility toggle chips
    seq=[header,oled,ctrl_top,cond_leaf,cond_gw,ota,dev]
    if plug: seq.append(plug)
    seq.append(ctrl_bottom)
    return {"type":"vertical-stack","view_layout":{"grid-column":"span 4"},"cards":seq}

def legend_card(nodes, present):
    ents=[{"type":"section","label":"the mesh"}]
    if "sensor.smol_mesh_channel" in present:
        for a,nm,ic in [("owner","the Seat (owner id)","mdi:crown"),("channel","elected channel","mdi:wifi"),("seq","mesh seq","mdi:counter")]:
            ents.append({"type":"attribute","entity":"sensor.smol_mesh_channel","attribute":a,"name":nm,"icon":ic})
    for e,nm,ic in [("binary_sensor.smol_mesh_reelecting","re-electing","mdi:crown-outline"),("binary_sensor.smol_mesh_asleep","mesh asleep","mdi:sleep")]:
        if e in present: ents.append({"entity":e,"name":nm,"icon":ic})
    ents.append({"type":"section","label":"sigils & bonds (bond=RSSI · adrift when ch≠mesh)"})
    for n in nodes:
        ents.append({"entity":f"binary_sensor.smol_{n['id']}_online","name":f"{'♛ ' if n['gate'] else ''}{n['name']} · id{n['id']}"})
        if f"sensor.smol_{n['id']}_rssi" in present: ents.append({"entity":f"sensor.smol_{n['id']}_rssi","name":"   ↳ bond (RSSI)","icon":"mdi:signal"})
        if f"sensor.smol_{n['id']}_peers" in present: ents.append({"entity":f"sensor.smol_{n['id']}_peers","name":"   ↳ peers","icon":"mdi:lan"})
    return {"type":"entities","title":"the mesh","show_header_toggle":False,"entities":ents,
            "card_mod":{"style":accent_top(PHOS)},"view_layout":{"grid-column":"span 5"}}

# ---------- FLEET-OTA row: staged build vs each node's RUNNING build# (+ live relay % / phase) ----------
# Uses sensor.smol_<id>_build (running, #40) vs sensor.smol_ota_staged; when a node lags the staged
# build it shows run **B** → **S** with the live relay % + phase chip. Scales with the fleet.
def forge_ota_md(nodes, present):
    out=["**fleet · staged vs running**",
         "staged **{% set s=states('sensor.smol_ota_staged') %}{{ s if s not in "+NAJ+" else '— none' }}**"]
    for n in nodes:
        I=str(n["id"]); noun=n["name"].split()[-1]; tag=" ⚑" if n["gate"] else ""
        row=("{% set na="+NAJ+" %}{% set s=states('sensor.smol_ota_staged') %}{% set b=states('sensor.smol_"+I+"_build') %}"
             "{% set ph=states('sensor.smol_"+I+"_ota_diag') %}{% set p=states('sensor.smol_"+I+"_ota_relaydiag') %}"
             "{% set on=is_state('binary_sensor.smol_"+I+"_online','on') %}{% set pm="+PMAP+" %}{% set pl=pm.get(ph,"+PDEF+") %}"
             "**id"+I+"** "+noun+tag+" — "
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
            rows.append({"entity":inst,"name":f"Install → id{I} {n['name'].split()[-1]}"+(" · canary / the Seat" if n["gate"] else ""),
                         "icon":"mdi:rocket-launch" if n["gate"] else "mdi:rocket-launch-outline"})
    return rows

def gen_topology(nodes, seat):
    W,H=680,300; cx,cy=W/2,H*0.40; F="ui-monospace,'DejaVu Sans Mono',monospace"
    P=[f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}">',
       '<defs><pattern id="dg" width="16" height="16" patternUnits="userSpaceOnUse"><circle cx="1.5" cy="1.5" r=".8" fill="#0f3a24"/></pattern>'
       '<radialGradient id="sg" cx="50%" cy="50%" r="50%"><stop offset="0%" stop-color="#5bff9a" stop-opacity=".5"/><stop offset="100%" stop-color="#5bff9a" stop-opacity="0"/></radialGradient></defs>',
       f'<rect width="{W}" height="{H}" fill="#020402"/><rect width="{W}" height="{H}" fill="url(#dg)"/>',
       f'<text x="{W-14}" y="22" text-anchor="end" font-family="{F}" font-size="12" fill="#2f7a4e">SHARED MESH</text>']
    leaves=[n for n in nodes if n["id"]!=seat["id"]]; m=len(leaves); ly=H*0.80
    for i,lf in enumerate(leaves):
        lx=W*0.16+(W*0.68)*(i/(m-1) if m>1 else .5); on=lf["on"]; col="#5bff9a" if on else "#ff6b6b"
        anim='<animate attributeName="opacity" values="0.9;0.5;0.9" dur="3s" repeatCount="indefinite"/>' if on else ''
        P.append(f'<line x1="{cx:.0f}" y1="{cy:.0f}" x2="{lx:.0f}" y2="{ly:.0f}" stroke="{col}" stroke-width="{3 if on else 1.5}"{"" if on else " stroke-dasharray=\"6 5\""} opacity="{.85 if on else .7}">{anim}</line>')
        P.append(f'<circle cx="{lx:.0f}" cy="{ly:.0f}" r="11" fill="#020402" stroke="{col}" stroke-width="{2.5 if on else 2}"/>')
        P.append(f'<text x="{lx:.0f}" y="{ly+27:.0f}" text-anchor="middle" font-family="{F}" font-size="16" font-weight="600" fill="{"#c9e8d2" if on else "#6f8f78"}">{esc(lf["name"])}</text>')
        P.append(f'<text x="{lx:.0f}" y="{ly+43:.0f}" text-anchor="middle" font-family="{F}" font-size="11" fill="{col}">id{lf["id"]} · {"attuned" if on else "offline"}</text>')
    P.append(f'<circle cx="{cx:.0f}" cy="{cy:.0f}" r="46" fill="url(#sg)"/>')
    P.append(f'<circle cx="{cx:.0f}" cy="{cy:.0f}" r="13" fill="#020402" stroke="#ffc24b" stroke-width="2.5"><animate attributeName="r" values="13;15.5;13" dur="2.6s" repeatCount="indefinite"/></circle>')
    P.append(f'<text x="{cx:.0f}" y="{cy+6:.0f}" text-anchor="middle" font-family="{F}" font-size="18" fill="#ffc24b">&#9819;</text>')
    P.append(f'<text x="{cx:.0f}" y="{cy-30:.0f}" text-anchor="middle" font-family="{F}" font-size="20" font-weight="600" fill="#c9e8d2">the Seat · id{seat["id"]}</text>')
    P.append(f'<text x="{cx:.0f}" y="{cy+34:.0f}" text-anchor="middle" font-family="{F}" font-size="12" fill="#ffc24b">{esc(seat["name"])} · GATE</text>')
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

async def main():
    view=yaml.safe_load(open("smol-control-scaffold.yaml"))
    async with websockets.connect(URI,max_size=16*1024*1024,ssl=SSLCTX) as ws:
        json.loads(await ws.recv()); await ws.send(json.dumps({"type":"auth","access_token":TOKEN})); await ws.recv()
        st={s["entity_id"]:s for s in (await rpc(ws,{"type":"get_states"}))["result"]}; present=set(st)
        ids=sorted(int(m.group(1)) for e in present if (m:=re.match(r"binary_sensor\.smol_(\d+)_online$",e))) or [7,8,9]
        online={i for i in ids if st.get(f"binary_sensor.smol_{i}_online",{}).get("state")=="on"}
        owner=st.get("sensor.smol_mesh_channel",{}).get("attributes",{}).get("owner")
        try: seat_id=int(owner)
        except (TypeError,ValueError): seat_id=min(online) if online else min(ids)
        nodes=[]
        for i in ids:
            meta=dict(KNOWN.get(i,{"name":f"Sigil id{i}","role":"leaf"}))
            meta.update(role=meta.get("role","leaf"),name=meta.get("name",f"Sigil id{i}"),gate=(i==seat_id),on=(i in online),id=i)
            nodes.append(meta)
        nodes.sort(key=lambda n:(not n["gate"],not n["on"],n["id"]))
        seat=next(n for n in nodes if n["id"]==seat_id)
        topo_url=serve("smol-topology.svg", gen_topology(nodes,seat))
        node_cards=[node_card(n["id"],n,present) for n in nodes]
        legend=legend_card(nodes,present)
        cards=view["cards"]; out=[]; done={"topo":0,"legend":0,"fleet":0,"forge":0,"install":0}
        for c in cards:
            if c.get("type")=="picture" and c.get("image")=="TOPO": c["image"]=topo_url; done["topo"]+=1; out.append(c)
            elif c.get("type")=="markdown" and c.get("content")=="LEGEND":
                lc=dict(legend); lc["view_layout"]=c.get("view_layout") or lc.get("view_layout"); done["legend"]+=1; out.append(lc)
            elif c.get("type")=="markdown" and c.get("content")=="FLEET":
                out.extend(node_cards); done["fleet"]+=1
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
        cfg["views"]=[v for v in cfg["views"] if v.get("title")!="smol Nodes" and v.get("path")!="smol-control"]+[view]
        s=await rpc(ws,{"type":"lovelace/config/save","url_path":DASH,"config":cfg})
        if not s.get("success"): print("!! SAVE FAILED",s); return
        r2=(await rpc(ws,{"type":"lovelace/config","url_path":DASH}))["result"]
        vv=next(x for x in r2["views"] if x.get("path")=="smol-control")
        span4=[c for c in vv["cards"] if (c.get("view_layout") or {}).get("grid-column")=="span 4" and c.get("type")=="vertical-stack"]
        print("SAVE ok · nodes:",ids,"Seat id",seat_id,"online",sorted(online))
        print("  node boxes spliced into view grid (span-4 vertical-stacks):",len(span4),"· done:",done)
        print("  each box:",[c.get("type") for c in span4[0]["cards"]] if span4 else "NONE")
if __name__=="__main__":
    asyncio.run(main())

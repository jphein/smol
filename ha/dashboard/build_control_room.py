#!/usr/bin/env python3
# smol · Control Room builder — MINIMAL FIX: un-nest the fleet (the black hole).
# JP screenshot: node cards were MISSING because they lived in a nested custom:grid-layout
# card that renders EMPTY. Fix = splice node cards DIRECTLY into the view grid (span 4, like
# glass/power/forge which render). Node cards stay LIVE mushroom boxes (header + OLED + entities).
# If mushroom still doesn't render un-nested → it's mushroom, swap to SVG faceplate then.
#   HA_TOKEN=<your-HA-long-lived-access-token> python3 build_control_room.py
import asyncio, json, os, re, subprocess, hashlib, yaml, websockets
try:
    from defusedxml.minidom import parseString as xml_parse
except ImportError:
    from xml.dom.minidom import parseString as xml_parse
URI="wss://homeassistant.local:8123/api/websocket"; TOKEN=os.environ["HA_TOKEN"]; DASH="dashboard-dashboard"
HA="user@homeassistant.local"; WWW="/config/www/luna-cards"; LOCAL="/local/luna-cards"
KNOWN={7:{"name":"Draconic Dominion","role":"the Seat","gate":True},
       8:{"name":"Eldritch Nexus","role":"leaf"},
       9:{"name":"Jade Herald","role":"leaf"}}
ACCENT="var(--accent-color)"; PHOS="var(--primary-color)"; VT="'VT323','IBM Plex Mono',monospace"
NAJ="['unavailable','unknown','none','None','']"
def esc(s): return str(s).replace("&","&amp;").replace("<","&lt;").replace(">","&gt;")
def accent_top(c): return ("ha-card{position:relative;overflow:hidden}ha-card:before{content:'';position:absolute;top:0;left:0;right:0;height:2px;"
                           "background:linear-gradient(90deg,transparent,%s,transparent);opacity:.55}"%c)

# ---------- node box = mushroom header + mushroom OLED + entities; span-4 in the VIEW grid ----------
def node_card(nid, meta, present):
    gate=meta["gate"]; I=str(nid); on=f"is_state('binary_sensor.smol_{I}_online','on')"
    # RSSI in EVERY node: leaves show dBm; gateway (no mesh-RSSI to itself) shows WiFi uplink
    has_rssi=f"sensor.smol_{I}_rssi" in present
    rssi_pip=(" · {{ states('sensor.smol_"+I+"_rssi') if states('sensor.smol_"+I+"_rssi') not in na else '—' }} dBm") if has_rssi else " · WiFi"
    hdr=("{% set on="+on+" %}{% set t=states('sensor.smol_"+I+"_temp') %}{% set v=states('sensor.smol_"+I+"_voltage') %}"
         "{% set na="+NAJ+" %}id"+I+" · "+meta["role"]+" · {{ '🟢 online' if on else '🔴 offline' }}"
         " · {{ t if t not in na else '—' }}° · {{ v if v not in na else '—' }}V"+rssi_pip)
    header={"type":"custom:mushroom-template-card","primary":meta["name"],"secondary":hdr,
            "icon":"mdi:crown" if gate else "mdi:chip",
            "icon_color":"amber" if gate else ("{{ 'green' if "+on+" else 'red' }}"),
            "badge_icon":"mdi:crown" if gate else "mdi:leaf",
            "badge_color":"amber" if gate else ("{{ 'green' if "+on+" else 'red' }}"),
            "card_mod":{"style":{".":accent_top(ACCENT if gate else PHOS)+"ha-card{border-radius:10px 10px 0 0;border-bottom:none}",
                "mushroom-state-info$":".primary{font-family:"+VT+";font-size:26px;line-height:.9}.secondary{font-size:11px}"}}}
    # mini-OLED shows the SCREEN's content (like the board): Grid→grid W, Batt→HV SOC, Clock→time, else temp
    scr="states('input_select.smol_"+I+"_screen')"
    oled_p=("{% set scr="+scr+" %}{% set t=states('sensor.smol_"+I+"_temp') %}{% set g=states('sensor.smol_display_grid') %}{% set na="+NAJ+" %}"
            "{% if not "+on+" %}—{% elif scr=='Grid' %}{{ g.split('|')[1] if '|' in g else '—' }}"
            "{% elif scr=='Batt' %}{{ states('sensor.ev_battery_soc') }}%{% elif scr=='Clock' %}{{ now().strftime('%H:%M') }}"
            "{% else %}{{ t if t not in na else '—' }}{% endif %}")
    oled_s=("{% set scr="+scr+" %}{{ scr|upper }} · {% if not "+on+" %}no link{% elif scr=='Grid' %}shared glass{% elif scr=='Batt' %}HV pack{% elif scr=='Clock' %}mesh time{% else %}live °F{% endif %}")
    oled={"type":"custom:mushroom-template-card","primary":oled_p,"secondary":oled_s,"icon":"mdi:blank",
          "card_mod":{"style":{".":("ha-card{background:#020402;border:1px solid var(--ha-card-border-color);border-radius:0;"
                "box-shadow:inset 0 0 12px rgba(0,0,0,.9);position:relative;overflow:hidden;margin-top:-2px}mushroom-shape-icon{display:none}"),
                "mushroom-state-info$":(".primary{font-family:"+VT+";font-size:44px;line-height:.8;color:var(--primary-color);"
                "text-shadow:0 0 7px rgba(91,255,154,.55)}.secondary{color:var(--primary-color);opacity:.7;font-size:10px}")}}}
    ents=[]
    def sec(l): ents.append({"type":"section","label":l})
    def row(eid,nm,icon=None):
        if eid in present:
            r={"entity":eid,"name":nm}
            if icon: r["icon"]=icon
            ents.append(r)
    sec("screen & mode")
    row(f"input_select.smol_{nid}_screen","default screen")
    row(f"input_select.smol_{nid}_page","page")
    row(f"input_button.smol_{nid}_apply",f"Apply → id{nid}","mdi:send")
    row(f"input_button.smol_{nid}_reset","Reset to board default","mdi:backup-restore")
    sec("readback")
    row(f"sensor.smol_{nid}_config","screen · page (commanded)","mdi:monitor-dashboard")
    # NOTE: sensor.smol_<id>_screen (LIVE current screen) omitted — it's 'unknown' until firmware
    # publishes smol/<id>/status = STAT|screen:page|build (design F4). Commanded config above is the
    # effective screen the node applies. Row returns when F4 ships the live readback.
    row(f"sensor.smol_{nid}_status","activity","mdi:pulse")
    row(f"sensor.smol_{nid}_rssi","bond (RSSI)","mdi:signal")
    row(f"sensor.smol_{nid}_rssi_band","bond band","mdi:signal-cellular-2")
    row(f"binary_sensor.smol_{nid}_resync","re-syncing","mdi:sync")
    sec("firmware")
    fw=next((e for e in present if re.match(rf"update\.smol_{nid}_.*_firmware$",e)),None)
    if fw: ents.append({"entity":fw,"name":"firmware (version + update)"})
    inst=f"input_button.smol_ota_install_{nid}"
    if inst in present: ents.append({"entity":inst,"name":"Install staged (canary)" if gate else "Install (when this leads)","icon":"mdi:rocket-launch"})
    ctrl={"type":"entities","show_header_toggle":False,"entities":ents,
          "card_mod":{"style":"ha-card{border-radius:0 0 10px 10px;border-top:none;margin-top:-1px}"}}
    return {"type":"vertical-stack","view_layout":{"grid-column":"span 4"},"cards":[header,oled,ctrl]}

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

# ---------- forge per-node OTA summary (generated markdown; scales) ----------
FORGE_ROW=("id__ID____TAG__ — {% if is_state('__FW__','on') %}**{{ state_attr('__FW__','installed_version') }} → {{ state_attr('__FW__','latest_version') }} ready**"
           "{% elif is_state('binary_sensor.smol___ID___online','on') %}✓ {{ state_attr('__FW__','installed_version') }} up-to-date"
           "{% else %}offline{% endif %}")
def forge_ota_md(nodes, present):
    out=["**per-node OTA**"]
    for n in nodes:
        I=str(n["id"]); fw=next((e for e in present if re.match(rf"update\.smol_{I}_.*_firmware$",e)),None)
        tag=" · canary" if n["gate"] else ""
        out.append(FORGE_ROW.replace("__ID__",I).replace("__FW__",fw).replace("__TAG__",tag) if fw else f"id{I}{tag} — n/a")
    return "\n\n".join(out)

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
    async with websockets.connect(URI,max_size=16*1024*1024) as ws:
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
        cards=view["cards"]; out=[]; done={"topo":0,"legend":0,"fleet":0,"forge":0}
        for c in cards:
            if c.get("type")=="picture" and c.get("image")=="TOPO": c["image"]=topo_url; done["topo"]+=1; out.append(c)
            elif c.get("type")=="markdown" and c.get("content")=="LEGEND":
                lc=dict(legend); lc["view_layout"]=c.get("view_layout") or lc.get("view_layout"); done["legend"]+=1; out.append(lc)
            elif c.get("type")=="markdown" and c.get("content")=="FLEET":
                out.extend(node_cards); done["fleet"]+=1
            else: out.append(c)
        view["cards"]=out
        def fill_forge(cs):                                   # FORGE_OTA is nested in the forge vertical-stack
            for c in cs:
                if c.get("type")=="markdown" and c.get("content")=="FORGE_OTA": c["content"]=forge_ota_md(nodes,present); done["forge"]+=1
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

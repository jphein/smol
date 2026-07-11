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
            "{% else %}{{ t if t not in na else '—' }}{% endif %}")
    oled_s=("{% set scr="+scr+" %}{{ scr|upper }} · {% if not "+on+" %}no link{% elif scr=='Grid' %}shared glass{% elif scr=='Batt' %}HV pack{% elif scr=='Clock' %}mesh time{% else %}live °F{% endif %}")
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
    fw=next((e for e in present if re.match(rf"update\.smol_{nid}_.*_update$",e)),None)
    if fw: bot.append({"entity":fw,"name":"firmware (version + update)"})
    inst=f"input_button.smol_ota_install_{nid}"
    if inst in present: bot.append({"entity":inst,"name":"Install staged (gateway consumes)","icon":"mdi:rocket-launch"})
    ctrl_bottom={"type":"entities","show_header_toggle":False,"entities":bot,
                 "card_mod":{"style":"ha-card{border-radius:0 0 10px 10px;border-top:none;margin-top:-1px;"+OP+"}"}}
    return {"type":"vertical-stack","view_layout":{"grid-column":"span 4"},"cards":[header,oled,ctrl_top,cond_leaf,cond_gw,ctrl_bottom]}

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
        I=str(n["id"]); fw=next((e for e in present if re.match(rf"update\.smol_{I}_.*_update$",e)),None)
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

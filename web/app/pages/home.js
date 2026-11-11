import {$,$$,pad,esc,store,isPhone,ICON,PEND} from '../core/util.js';
import {api} from '../core/api.js';
import {VMS,PASSKEYS,ROOM_ORDER,ROOMS,CAT,DEVINFO,LAST,PREFS,PLUGINS,setPrefs,slug,roomByName,roomById,byTarget,classOf,capOf,TGT,S,isOwner,NR,D,T,crashCount,reporting} from '../core/store.js';
import {status,cameraOnHome} from '../core/status.js';
import {lean,bulbLight,paintScene,queueHighlights} from '../ui/roomlight.js';
import {raysKick,refractMaps} from '../ui/glass.js';
import {PENDING,NORESP,scenePressed} from '../core/actions.js';
import {toast} from '../ui/toast.js';
import {movieRoom,scenePending,runScene} from '../ui/scenes.js';
import {wireTile} from '../ui/gestures.js';
import {QDONE,qTell,paintQDone} from '../core/guards.js';
import {SHEET,openSheet,closeSheet,row,roomOf,sessionRow} from '../ui/sheets.js';
import {said,parse,suggestions,execute} from '../swifty/parser.js';
import {askSwifty} from '../swifty/chat.js';
import {renderWall} from '../ui/dock.js';
import {renderRooms} from '../ui/views.js';
import {paintTvBar} from '../cards/tv.js';
import {hasCard,cardSheet} from '../cards/index.js';
import {genericTile} from '../cards/generic.js';
import {SHEETS} from '../main.js';

/* ============================================================ rendering the first screen */
const COMP_SINCE=v=>{if(v.transition)return v.transition[0].toUpperCase()+v.transition.slice(1)+'…';if(!v.known)return NR;if(v.state!=='running')return 'Off';const m=Math.floor((Date.now()-v.since)/60e3);return 'On for '+(m>=60?Math.floor(m/60)+' h '+pad(m%60)+' m':m+' min')};
const COMP_SUB=v=>v.transition?'This takes a moment':!v.known?(v.reason||'No answer'):v.state==='running'?'Answering normally':'Not running';
const bulbById=id=>D.bulbs.find(b=>b.id===+id);
const bulbName=b=>b.name;
const roomBulbs=room=>D.bulbs.filter(b=>roomOf('bulb'+b.id)===room);
function lightsSummary(bulbs){
  const src=bulbs||D.bulbs;const known=src.filter(b=>b.known);
  const lit=known.filter(b=>bulbLight(b).on>0.5);
  const n=lit.length;const avg=n?Math.round(lit.reduce((s,b)=>s+bulbLight(b).bri,0)/n):0;
  let hx=0,hy=0;for(const b of lit){const L=bulbLight(b);hx+=Math.cos(b.hue/180*Math.PI)*L.bri;hy+=Math.sin(b.hue/180*Math.PI)*L.bri}
  const hue=n?((Math.atan2(hy,hx)*180/Math.PI)+360)%360:270;
  const state=!src.length?'No lights yet':!known.length?NR:n===0?'All off':n===known.length?(known.length===1?'On':'All on'):n+' of '+known.length+' on';
  return {n,avg,hue,total:src.length,state};
}
function tileInner(kind,room){
  if(kind==='lights'){const L=lightsSummary(room?roomBulbs(room):null);return `<div class="inner"><div class="head"><span class="name" style="display:flex;align-items:center;gap:10px">${ICON('bulb')}${esc(room||'Lights')}</span></div><div class="foot"><span class="state">${L.state}</span><span class="big num">${L.n?L.avg+'%':''}</span></div></div>`}
  const v=lean('speaker','vol',D.speaker.vol);const sp=!!D.speaker.target,tvHere=!!D.tv.target;
  const words=[tvHere?(D.tv.known?(D.tv.on?'TV on':'TV off'):'TV not reporting'):sp&&!D.speaker.known?'Not reporting':sp?'Speaker':'',sp&&D.speaker.known&&D.speaker.mute?(tvHere?'muted':'Muted'):''].filter(Boolean).join(' · ');
  return `<div class="inner"><div class="head"><span class="name" style="display:flex;align-items:center;gap:10px">${ICON('speaker')}${sp&&tvHere?'Music &amp; TV':tvHere?'TV':'Music'}</span></div><div class="foot"><span class="state">${words}</span><span class="big num">${D.speaker.known?Math.round(v):'—'}</span></div></div>`;
}
function paintTile(el,kind,room){
  if(!el||el.classList.contains('dragging'))return;
  const bulbs=room?roomBulbs(room):D.bulbs;
  if(kind==='lights')el.setAttribute('aria-label',(room?room+' lights':'Lights')+' brightness');
  const L=lightsSummary(room?bulbs:null);
  const p=kind==='lights'?(L.n?L.avg:0):lean('speaker','vol',D.speaker.vol);
  const pend=kind==='lights'?(!!PENDING.lights||(room&&!!PENDING['room-'+slug(room)])||bulbs.some(b=>PENDING['bulb'+b.id])):!!PENDING.speaker;
  if(!el.firstChild){el.innerHTML=`<div class="clip under"><div class="glow"></div></div><div class="live"></div><div class="clip over"><div class="fill"><div class="dup"></div></div></div><span class="pending-ring" hidden>${PEND}</span>`}
  const inner=tileInner(kind,room);
  $('.live',el).innerHTML=inner;$('.dup',el).innerHTML=inner;
  el.style.setProperty('--p',p.toFixed(1));
  /* an empty room has nothing to slide: it is a plain button that opens the room */
  if(room&&!bulbs.length){el.setAttribute('role','button');['aria-valuemin','aria-valuemax','aria-valuenow','aria-valuetext'].forEach(a=>el.removeAttribute(a))}
  else{el.setAttribute('role','slider');el.setAttribute('aria-valuemin','0');el.setAttribute('aria-valuemax','100');el.setAttribute('aria-valuenow',Math.round(p));el.setAttribute('aria-valuetext',kind==='lights'?(L.n?L.state+', '+L.avg+'%':L.state):'Volume '+Math.round(p))}
  $('.pending-ring',el).hidden=!pend;
  if(kind==='lights'){el.style.setProperty('--gh',L.hue.toFixed(0));el.style.setProperty('--ga',L.n?(0.12+0.3*L.avg/100).toFixed(2):'0');$('.fill',el).classList.toggle('none',!L.n)}
  const fails=NORESP.filter(n=>n.key===(kind==='lights'?'lights':'speaker')||(kind==='lights'&&(/^bulb/.test(n.key)||(room&&n.key==='room-'+slug(room)))));
  el.style.setProperty('--hlc',fails.length?'var(--red)':'');
}
function dupWidths(){for(const el of $$('.tile .dup'))el.style.setProperty('--w',el.closest('.tile').clientWidth+'px');for(const el of $$('.slider .sfill .slab'))el.style.setProperty('--w',el.closest('.slider').clientWidth+'px')}
/* a computer's word: what it is doing, or how many times it crashed today */
function compWord(key,on,busy,busyWord,known){const n=reporting(key)?crashCount(key):0;if(busy)return busyWord;if(n)return n+' crash'+(n===1?'':'es');if(!known)return NR;return on?'On':'Off'}
function compTile(v){
  const on=v.known&&v.state==='running';const busy=!!v.transition;
  const word=compWord(v.id,on,busy,busy?v.transition[0].toUpperCase()+v.transition.slice(1)+'…':'',v.known);
  return `<button class="g tile h112 comp press ${on?'on':''} ${busy?'busy':''}" data-sheet="comp:${esc(v.id)}" data-key="${esc(v.id)}" id="t-${esc(v.id)}" aria-label="${esc(v.name)}, ${esc(busy?v.transition:v.known?(on?'on':'off'):'not reporting')}">
    <span class="head">${ICON(v.icon)}<span class="pending-ring" ${busy||PENDING[v.id]?'':'hidden'} style="position:static">${PEND}</span></span>
    <span class="inner"><span class="name"><span class="long">${esc(v.name)}</span><span class="shrt" hidden>${esc(v.short)}</span></span><span class="word"><i class="dot"></i>${esc(word)}</span></span></button>`;
}
/* the host the computers run on, by the name the hub has for it */
function hostTile(){
  const busy=!!PENDING.host;const on=T.hostKnown&&T.hostUp;const alert=!busy&&on&&crashCount('host')>0;
  const word=compWord('host',on,busy,'Working…',T.hostKnown);
  return `<button class="g tile h112 comp press ${on?'on':''} ${busy?'busy':''} ${alert?'alert':''}" data-sheet="host" data-key="host" id="t-host" aria-label="${esc(T.hostName)}, the host the computers run on, ${busy?'working':alert?esc(word):T.hostKnown?(on?'on':'off'):'not reporting'}">
    <span class="head">${ICON('tech')}<span class="pending-ring" ${busy?'':'hidden'} style="position:static">${PEND}</span></span>
    <span class="inner"><span class="name">${esc(T.hostName)}</span><span class="word"><i class="dot"></i>${esc(word)}</span></span></button>`;
}
/* a room as a tile that drags like the Lights tile */
function roomTileEl(r,cls){
  const d=document.createElement('div');d.className='g lens tile h220 lights room press';d.dataset.lens=(cls||'rt')+'-'+slug(r);d.id=(cls||'rt')+'-'+slug(r);
  d.tabIndex=0;d.setAttribute('aria-label',r+' lights');d.dataset.room=r;
  wireTile(d,'lights',r);return d;
}
/* the room a person pinned is where Home opens for them */
const pinnedRoom=()=>{for(const id of PREFS.pinned_rooms||[]){const r=roomById(id);if(r)return r.name}return null};
/* the rooms Home can show, and which one it is showing: the last pick on this device, then the pinned room, then everything */
const pickable=()=>{const used=Object.values(ROOMS);return ROOM_ORDER.filter(r=>used.includes(r))};
/* with a room picked, Home shows what is in it and whatever has no room yet */
const inView=key=>{const here=pickedRoom();const r=roomOf(key);return !here||!r||r===here};
function pickedRoom(){const rooms=pickable();const saved=store.get('home-room',null);if(saved==='')return null;const r=saved?roomById(saved):null;if(r&&rooms.includes(r.name))return r.name;const p=pinnedRoom();return p&&rooms.includes(p)?p:null}
function setPick(name){const r=name?roomByName(name):null;store.set('home-room',r?r.id:'');render()}
function paintRoomPick(el){const rooms=pickable();el.hidden=!rooms.length;if(!rooms.length)return;const cur=pickedRoom();const html=[null].concat(rooms).map(r=>`<button class="pill g" role="radio" aria-checked="${r===cur}" tabindex="${r===cur?0:-1}" data-pick="${r?esc(r):''}">${esc(r||'All')}</button>`).join('');if(el.dataset.h!==html){el.innerHTML=html;el.dataset.h=html}}
function wireRoomPick(el){el.addEventListener('click',e=>{const b=e.target.closest('[data-pick]');if(b)setPick(b.dataset.pick||null)});
  el.addEventListener('keydown',e=>{const d={ArrowRight:1,ArrowDown:1,ArrowLeft:-1,ArrowUp:-1}[e.key];if(!d)return;e.preventDefault();const bs=$$('[data-pick]',el);const n=bs[(bs.indexOf(document.activeElement)+d+bs.length)%bs.length];setPick(n.dataset.pick||null);const f=$('[aria-checked="true"]',el);if(f)f.focus()})}
function plugTile(p){const pend=!!PENDING[p.target];const word=p.known?(p.on?'On':'Off'):(p.reason||NR);const room=pickedRoom()?null:roomOf(p.target);const words=word+(room?' · '+room:'');return `<div class="g tile h112 comp plugt ${p.on?'on':''}" id="t-${esc(p.target)}"><button class="plugopen" data-sheet="plug:${esc(p.target)}" aria-label="${esc(p.name)}, ${esc(words)}. Name and room"><span class="name">${esc(p.name)}</span><span class="word"><i class="dot"></i>${esc(words)}</span></button><button class="toggle ${pend?'pending':''}" role="switch" aria-checked="${!!p.on}" aria-label="${esc(p.name)}" data-plug="${esc(p.target)}" ${p.known?'':'disabled'}></button></div>`}
const AC_MODES_SHOWN=[['auto','Auto'],['cool','Cool'],['heat','Heat'],['dry','Dry'],['fan','Fan']],AC_FANS_SHOWN=[['auto','Auto'],['quiet','Quiet'],['low','Low'],['medium_low','Med-low'],['medium','Medium'],['medium_high','Med-high'],['high','High']];
const AC_LOW=17,AC_HIGH=30,AC_DOING={auto:'Automatic',cool:'Cooling',heat:'Heating',dry:'Drying',fan:'Fan only'};
/* what this air conditioner says it accepts: its own range, modes and fan speeds, with the words for the ones this page knows */
const acRange=t=>{const c=capOf(t,'temperature');return c&&c.min!=null&&c.max!=null?[c.min,c.max]:[AC_LOW,AC_HIGH]};
const acChoices=(t,cap,list,known)=>{const c=capOf(t,cap);const offered=c&&Array.isArray(c[list])?c[list]:null;if(!offered)return known;return offered.map(w=>known.find(k=>k[0]===w)||[w,w.replace(/_/g,' ').replace(/^./,x=>x.toUpperCase())])};
const acModes=t=>acChoices(t,'mode','modes',AC_MODES_SHOWN),acFans=t=>acChoices(t,'fan','speeds',AC_FANS_SHOWN);
function acWord(c){if(!c.known)return c.reason||NR;if(!c.on)return 'Off';const to=c.mode==='fan'||c.temp==null?'':' to '+c.temp+'°';return (AC_DOING[c.mode]||'On')+to}
function acTile(c){const pend=!!PENDING[c.target];const room=pickedRoom()?null:roomOf(c.target);const inside=c.known&&c.indoor!=null?c.indoor+'° inside':'';const words=[acWord(c),inside,room].filter(Boolean).join(' · ');return `<div class="g tile h112 comp plugt ${c.on?'on':''}" id="t-${esc(c.target)}"><button class="plugopen" data-sheet="ac:${esc(c.target)}" aria-label="${esc(c.name)}, ${esc(words)}. Controls, name and room"><span class="name">${esc(c.name)}</span><span class="word"><i class="dot"></i>${esc(words)}</span></button><button class="toggle ${pend?'pending':''}" role="switch" aria-checked="${!!c.on}" aria-label="${esc(c.name)}" data-ac="${esc(c.target)}" ${c.known?'':'disabled'}></button></div>`}
const petWord=()=>PENDING.feeder?'feeding…':PENDING.litter?'cleaning…':D.feeder.known?(D.feeder.last?'fed '+D.feeder.last:'no feed yet'):'not reporting';
function smallCaps(){
  const p=T.printer;
  const pr=!p.known?(S.live&&p.was?'not responding':'not reporting'):p.state==='printing'?Math.round(p.pct)+'% done':p.state==='paused'?'paused':p.state==='stopped'?'stopped':p.state;
  const prLost=S.live&&p.was&&!p.known;
  const petFail=NORESP.find(n=>n.key==='feeder'||n.key==='litter');
  const camBad=T.camKnown&&!T.camOk;
  const here=t=>!!t&&inView(t);
  const items=[
    {k:'pets',icon:petFail?'warn':'paw',l:'Pets',st:petFail?'no answer':petWord(),alert:!!petFail,show:here(D.feeder.target)||here(D.litter.target)},
    {k:'camera',icon:camBad?'warn':'camera',l:'Camera',st:T.camKnown?(T.camOk?'answering':'no answer'):'not reporting',alert:camBad,show:here(TGT.camera)}
  ];
  /* the printer is the owner's: family sees lights, music, TV, pets and the camera */
  if(isOwner())items.push({k:'printer',icon:prLost?'warn':'printer',l:'Printer',st:pr,alert:prLost,show:here(TGT.printer)});
  /* a device of a class this page has no card for gets the generic one */
  const others=Object.keys(DEVINFO).filter(t=>!hasCard(classOf(t))&&inView(t)).sort(byTarget);
  return items.filter(i=>i.show).map(i=>`<button class="g cap press ${i.alert?'alert':''}" data-sheet="${i.k}" id="c-${i.k}" data-key="${i.k}" ${i.alert?'aria-describedby="title"':''}>${ICON(i.icon)}<span class="lbl">${i.l}</span><span class="st num">${esc(i.st)}</span></button>`).join('')+others.map(genericTile).join('');
}
/* the header shrinks once the page has scrolled. The height it loses goes back in as a margin, so the page stays the same length: a shorter page near its end pulls the scroll back up, which would grow the header, which would push the scroll down again, over and over. It grows again only near the very top, for the same reason */
let topLost=0;
function compactTop(){const top=$('#top');const was=top.classList.contains('compact');const now=was?scrollY>4:scrollY>28;if(now===was)return;
  if(now){const h=$('h1.title',top);const keep=h.style.transition;h.style.transition='none';const full=top.offsetHeight;top.classList.add('compact');topLost=Math.max(0,full-top.offsetHeight);top.classList.remove('compact');void top.offsetHeight;h.style.transition=keep}
  top.classList.toggle('compact',now);top.style.marginBottom=now?topLost+'px':''}
function paintScenes(root){
  /* Movie dims the TV's room, so that is the one room whose row has it */
  const room=pickedRoom();const movie=!!room&&room===movieRoom();const names=room?(movie?['Movie','Bright','Relax','Off']:['Bright','Relax','Off']):['Movie','Evening','Lights off'];root.setAttribute('aria-label',room?room+' scenes':'Lights');root.classList.toggle('four',names.length===4);root.hidden=!!room&&!roomBulbs(room).length;
  const active=movie&&scenePressed(null)==='Movie'?'Movie':scenePressed(room);const lobes=$$('.ind i',root);const segs=$$('.seg',root);
  segs.forEach((b,i)=>{b.hidden=lobes[i].hidden=!names[i];if(!names[i]){b.setAttribute('aria-pressed','false');lobes[i].classList.remove('on');return}b.dataset.scene=names[i];const on=active===b.dataset.scene;b.setAttribute('aria-pressed',String(on));lobes[i].classList.toggle('on',on);const pend=scenePending(b.dataset.scene,b.dataset.scene==='Movie'?null:room);const txt=esc(b.dataset.scene)+(pend?'<span class="pend" style="position:absolute;right:14px"></span>':'');if(b.innerHTML!==txt)b.innerHTML=txt});
}
function render(){
  paintScene();
  document.body.classList.toggle('owner',isOwner());compactTop();
  const st=status();
  const title=$('#title');if(title.textContent!==st.text){title.textContent=st.text}
  $('#subtitle').textContent=st.hint;
  const dry=S.booted&&!S.live;const sn=$('#simnote');sn.hidden=!dry;if(dry)sn.textContent='Dry-run. Nothing on this page is sent to any device.';$('#wall-sim').textContent=dry?'Dry-run: nothing is sent.':'';
  paintRoomPick($('#roompick'));paintScenes($('#scenes'));paintTvBar();
  const nowRoom=pickedRoom(),tl=$('#t-lights'),tm=$('#t-music'),mainRow=tl.parentElement;tl.hidden=!!nowRoom&&!roomBulbs(nowRoom).length;tm.hidden=!((D.speaker.target&&inView(D.speaker.target))||(D.tv.target&&inView(D.tv.target)));mainRow.hidden=tl.hidden&&tm.hidden;mainRow.classList.toggle('one',tl.hidden||tm.hidden);
  const mainSig=tl.hidden+'|'+tm.hidden;if(mainRow.dataset.sig!==mainSig){mainRow.dataset.sig=mainSig;refractMaps();dupWidths()}
  paintTile(tl,'lights',nowRoom);paintTile(tm,'music');
  const shown=D.plugs.filter(p=>inView(p.target));const pl=$('#plugs');pl.hidden=!shown.length;const ph=shown.map(plugTile).join('');if(pl.dataset.h!==ph){pl.innerHTML=ph;pl.dataset.h=ph}
  const acShown=D.acs.filter(c=>inView(c.target));const al=$('#acs');al.hidden=!acShown.length;const ah=acShown.map(acTile).join('');if(al.dataset.h!==ah){al.innerHTML=ah;al.dataset.h=ah}
  const comps=$('#comps');const ch=VMS.filter(v=>v.home&&inView(v.id)).map(compTile).join('')+(TGT.host&&inView(TGT.host)?hostTile():'');comps.hidden=!isOwner()||!ch;if(comps.dataset.h!==ch){comps.innerHTML=ch;comps.dataset.h=ch}
  const sm=smallCaps();$('#smalls').hidden=!sm;if($('#smalls').dataset.h!==sm){$('#smalls').innerHTML=sm;$('#smalls').dataset.h=sm}
  $$('#comps .name .long').forEach(e=>e.hidden=isPhone());$$('#comps .name .shrt').forEach(e=>e.hidden=!isPhone());
  if(S.view==='rooms')renderRooms();
  if(S.wall)renderWall();
  if(SHEET.update)SHEET.update();
  dupWidths();queueHighlights();raysKick();
}


/* the integrations row on More: how many, once the hub has said */
const integrationsWord=()=>{if(!PLUGINS.known)return 'Add, set up again and remove';const n=PLUGINS.instances.length;return n+(n===1?' integration':' integrations')+' · '+Object.keys(DEVINFO).length+' devices'};
const homeSheets={
  /* More is one page for everyone: search and commands at the top, then the places that are not on Home */
  more(arg,opts){
    const owner=isOwner();
    const rows=[row('rooms','Rooms','Lights by room, scenes and pins',null,'data-act="rooms"'),row('heart','Favourites','What you keep close',null,'data-open="favourites"')];
    if(TGT.camera&&!cameraOnHome())rows.push(row('camera','Camera','Whether it answers',null,'data-open="camera"'));
    rows.push(row('tech','Devices','Every device, by kind',null,'data-open="devices"'));
    if(S.assistant)rows.push(row('swifty','Ask Swifty','Chat, control the home, look things up',null,'data-open="swifty"'),row('person','Swifty’s memory','What Swifty knows about you and the home',null,'data-open="memory"'));if(owner)rows.push(row('history','History','Who asked for what, and what answered',null,'data-open="history"'),row('key','Passkeys',PASSKEYS.length+(PASSKEYS.length===1?' passkey can':' passkeys can')+' sign in',null,'data-open="passkeys"'),row('person','People','Who can come in',null,'data-open="people"'),row('puzzle','Integrations',integrationsWord(),null,'data-open="integrations"'));
    if(owner&&TGT.host)rows.push(row('tech',esc(T.hostName),'The host the computers run on: health and readings',null,'data-open="host"'));
    if(owner&&(TGT.host||TGT.telemetry))rows.push(row('plus','Add machine','A computer on a host, another host, or an agent',null,'data-open="addmachine"'));
    rows.push(row('wall','Wall screen','Preview the tablet on the wall',null,'data-act="wall"'));
    const html=`<input class="field" id="q" type="search" placeholder="${S.assistant?'Search, or ask Swifty anything':owner?'Search, or type a command like “dim the lights”':'Search, or type “dim the bedroom lights”'}" aria-label="Search or command" autocomplete="off" autocapitalize="off">
      <div class="qdone" id="q-done" hidden></div><div id="q-answer" hidden></div><div class="sugg" id="q-sugg"></div>
      <div id="more-rest" style="display:flex;flex-direction:column;gap:18px">${sessionRow()}<div class="rows">${rows.join('')}</div></div>`;
    return {title:'More',html,mount(root){const q=$('#q',root);const sugg=$('#q-sugg',root);const rest=$('#more-rest',root);let sel=-1;
      const paint=()=>{const said=q.value.trim().replace(/^swifty[,:\s]*/i,'');const list=suggestions(q.value).concat(S.assistant&&said.length>1?[{q:'swifty, '+said,l:'Ask Swifty: '+said,icon:'swifty',k:''}]:[]);sugg.innerHTML=list.map((s,i)=>`<button data-q="${esc(s.q)}" class="${i===sel?'sel':''}">${ICON(s.icon||'search')}<span>${esc(s.l)}</span><small>${esc(s.k||'')}</small></button>`).join('');$$('[data-q]',sugg).forEach(b=>b.onclick=()=>{go(b.dataset.q)});
        const typing=!!q.value.trim();rest.hidden=typing;const r=typing?parse(q.value):null;const ans=$('#q-answer',root);
        if(r&&r.kind==='query'){ans.hidden=false;ans.innerHTML=`<div class="answer">${esc(r.answer)}</div>`}
        else if(r&&r.kind==='none'&&q.value.trim().length>2&&(r.why||!S.assistant&&!list.length)){ans.hidden=false;ans.innerHTML=`<div class="answer">${esc(r.why||(owner?'Not understood. Try “volume 20”, “movie”, “feed the pets” or “open history”.':'Not understood. Try “dim the bedroom lights”, “volume 20” or “feed the pets”.'))}</div>`}
        else ans.hidden=true};
      /* a command runs at once and the field empties for the next one; what is not understood goes to Swifty to work out */
      const go=text=>{const t=text.trim();if(!t)return;const named=/^swifty\b/i.test(t),r=parse(t);
        if(S.assistant&&(named||(r.kind==='none'&&!r.why))){askSwifty(t.replace(/^swifty[,:\s]*/i,''),{command:!named});return}
        QDONE.clear();const ok=execute(t,{channel:'typed',now:true,stay:true,tell:qTell});if(ok===false){q.value=text;paint();return}
        if(SHEET.open&&SHEET.type==='more'&&r.kind!=='nav'){q.value='';sel=-1;paint();q.focus({preventScroll:true})}};
      q.addEventListener('input',()=>{sel=-1;paint()});
      q.addEventListener('keydown',e=>{const n=$$('[data-q]',sugg).length;if(e.key==='ArrowDown'){e.preventDefault();sel=Math.min(n-1,sel+1);paint()}else if(e.key==='ArrowUp'){e.preventDefault();sel=Math.max(-1,sel-1);paint()}else if(e.key==='Enter'){e.preventDefault();const b=$$('[data-q]',sugg)[sel];go(b?b.dataset.q:q.value)}});
      if(Date.now()-(QDONE.at||0)>60000)QDONE.clear();paintQDone();paint();if(opts.focus==='field'||(!isPhone()&&opts.focus!=='keep'))setTimeout(()=>q.focus({preventScroll:true}),120)}};
  },
  favourites(){
    const scenes=[['evening','Evening'],['focus','Focus'],['all_off','Lights off'],['movie','Movie']];
    const label=t=>{const s=scenes.find(x=>x[0]===t);return s?s[1]:(LAST[t]&&LAST[t].name)||t};
    const devices=Object.keys(LAST).filter(t=>classOf(t)!=='telemetry').sort((a,b)=>label(a).localeCompare(label(b)));
    /* a kept device opens the page its card has */
    const open=t=>{const s=scenes.find(x=>x[0]===t);if(s){closeSheet();runScene(s[1],{channel:'touch'});return}if(LAST[t]||DEVINFO[t])openSheet(cardSheet(t));else toast('That one is not here any more.','')};
    const line=(t,l)=>`<div class="rowi"><div class="t"><b>${esc(l)}</b></div><button class="toggle" role="switch" aria-checked="${PREFS.favourites.includes(t)}" aria-label="Keep ${esc(l)}" data-fav="${esc(t)}"></button></div>`;
    const html=`${PREFS.favourites.length?`<div class="rows">${PREFS.favourites.map(t=>row('heart',esc(label(t)),null,null,`data-favgo="${esc(t)}"`)).join('')}</div>`:'<p class="quiet">Nothing kept yet.</p>'}
      <h3 class="roomhead" style="font-size:17px">Scenes</h3><div class="rows">${scenes.map(([t,l])=>line(t,l)).join('')}</div>
      <h3 class="roomhead" style="font-size:17px">Devices</h3><div class="rows">${devices.map(t=>line(t,label(t))).join('')}</div>`;
    return {title:'Favourites',html,mount(root){
      $$('[data-favgo]',root).forEach(b=>b.onclick=()=>open(b.dataset.favgo));
      $$('[data-fav]',root).forEach(b=>b.onclick=async()=>{const t=b.dataset.fav;const list=PREFS.favourites.includes(t)?PREFS.favourites.filter(x=>x!==t):PREFS.favourites.concat([t]);b.classList.add('pending');try{const r=await api('PUT','/me/favourites',{targets:list});setPrefs(r.preferences||PREFS);openSheet('favourites',null,{replace:true,focus:'keep'})}catch(e){b.classList.remove('pending');toast(e.message,'fail')}})}};
  },
  search(arg,opts){return SHEETS.more(arg,Object.assign({focus:'field'},opts||{}))},
};

export {COMP_SINCE,COMP_SUB,bulbById,bulbName,roomBulbs,lightsSummary,paintTile,dupWidths,roomTileEl,pinnedRoom,pickedRoom,setPick,paintRoomPick,wireRoomPick,inView,AC_MODES_SHOWN,AC_FANS_SHOWN,AC_LOW,AC_HIGH,acRange,acModes,acFans,acWord,petWord,compactTop,paintScenes,render,homeSheets};

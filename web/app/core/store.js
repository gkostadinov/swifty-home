import {clamp} from './util.js';
import {api,enc} from './api.js';
import {render} from '../pages/home.js';
import {wantApply} from './actions.js';
import {renderRooms} from '../ui/views.js';
import {guestsOf,kindsOf,energyOf} from './hostdata.js';

/* ============================================================ what we know: only what the API has told us */
let VMS=[],HOSTS=[],PASSKEYS=[],USERS=[],INVITES=[],ROOMDATA=[],ROOM_NAMES=[],ROOM_ORDER=[],ROOMS={};
let CAT={},DEVINFO={},LAST={},PREFS={favourites:[],pinned_rooms:[]},HEALTHDATA=null,AUDIT=[];
/* the house's scenes, the integrations (the owner's), and the owner's notes on each device, by target */
let SCENEDEFS=[],PLUGINS={known:false,plugins:[],instances:[]};const NOTES={};
/* another module cannot assign this binding, so new preferences come in through here */
function setPrefs(p){PREFS=p}
const AUDITLOAD={at:0,busy:false};
const vmById=id=>VMS.find(v=>v.id===id||v.key===id||String(v.vmid)===String(id));
const slug=s=>String(s).toLowerCase().replace(/[^a-z0-9]+/g,'-');
const escRe=s=>String(s).replace(/[.*+?^${}()|[\]\\]/g,'\\$&');
const roomByName=n=>ROOMDATA.find(r=>r.name===n)||null;
const roomById=id=>ROOMDATA.find(r=>r.id===id)||null;

/* ============================================================ devices by class: what a device is comes from the API, never from its name */
const byTarget=(a,b)=>String(a).localeCompare(String(b),undefined,{numeric:true});
const classOf=t=>{const d=DEVINFO[t];if(d&&d.class)return d.class;const s=LAST[t]&&LAST[t].state;return s&&s.kind||null};
const capsOf=t=>(t&&DEVINFO[t]&&DEVINFO[t].capabilities)||[];
const capOf=(t,cap)=>capsOf(t).find(c=>c.cap===cap)||null;
const hasCap=(t,cap)=>!!capOf(t,cap);
const targetsOf=cls=>[...new Set(Object.keys(DEVINFO).concat(Object.keys(LAST)))].filter(t=>classOf(t)===cls).sort(byTarget);
const nameOf=(t,dflt)=>(t&&DEVINFO[t]&&DEVINFO[t].name)||(t&&LAST[t]&&LAST[t].name)||dflt||t||'';
/* the one device of a class that Home and its page are about: the first, in target order, that can do what its card is for */
const TGT={speaker:null,tv:null,feeder:null,litter:null,printer:null,host:null,router:null,camera:null,telemetry:null};
const PRIMARY={speaker:'volume',tv:'power',telemetry:'metrics'};
function pickPrimaries(){for(const k of Object.keys(TGT)){const all=targetsOf(k);const want=PRIMARY[k];TGT[k]=(want&&all.find(t=>hasCap(t,want)))||all[0]||null}}
/* the plugin behind a device, as the integrations list names it (the owner's), and its instance */
const pluginOf=t=>{const d=DEVINFO[t];return d?PLUGINS.plugins.find(p=>p.id===d.plugin)||null:null};
const pluginLabel=t=>{const p=pluginOf(t);return p?(p.name||p.id):''};

/* ============================================================ state */
const S={view:'fav',locked:true,signedIn:false,wall:false,live:false,session:null,booted:false};
const person=()=>{const s=S.session;return {id:s?s.user_id:'',name:s?s.user_name:'',role:s?s.role:'family'}};
const isOwner=()=>!!S.session&&S.session.role==='owner';
const NR='Not reporting';
const D={bulbs:[],plugs:[],acs:[],speaker:{known:false,target:null,name:'Speaker',vol:0,mute:false,source:'',reason:''},tv:{known:false,target:null,name:'TV',on:false,input:'',reason:''},feeder:{known:false,target:null,name:'Feeder',last:null,portions:0,reason:''},litter:{known:false,target:null,name:'Litter box',last:null,busy:false,reason:''}};
const T={hostKnown:false,hostWas:false,hostUp:false,bootAt:0,hostName:'Host',wgKnown:false,wgOk:false,wgReason:'',routerName:'Router',camKnown:false,camWas:false,camOk:false,camReason:'',
  printer:{known:false,was:false,state:'unknown',nozzle:null,bed:null,layer:null,layers:null,pct:null,reason:'',lostAt:null,name:'Printer'},
  pkgW:null,pkgC:null,cpu:null,memUsed:null,cores:null,
  /* what the collector says about the machine's threads, the computers on it and its energy: all of it from the snapshot */
  memTotal:null,coreKinds:null,guests:[],energy:null};
const num=v=>typeof v==='number'&&isFinite(v)?v:null;
const stateOf=t=>{const d=t&&LAST[t];return d&&d.available&&d.state?d.state:null};
const reasonOf=t=>{const d=t&&LAST[t];return d&&!d.available?(d.simulated?'Dry-run: no device was contacted.':(d.error||NR)):''};
const usable=t=>{const d=t&&DEVINFO[t];return !d||d.enabled!==false};
/* what a TV calls its inputs, and the words on the buttons */
const TV_LABEL={'HDMI 1':'Input 1','HDMI 2':'Input 2','HDMI 3':'Input 3','HDMI 4':'Input 4','Live TV':'Live TV'};
const tvLabel=w=>TV_LABEL[w]||w;
const tvWire=l=>((capOf(TGT.tv,'inputs')||{}).inputs||Object.keys(TV_LABEL)).find(k=>tvLabel(k)===l)||Object.keys(TV_LABEL).find(k=>TV_LABEL[k]===l)||l;
/* lights are numbered in target order, once, so a light keeps its number while the page is open */
const BULBID={};let nextBulb=1;
const keyOfTarget=t=>{const id=BULBID[t];return id&&classOf(t)==='light'?'bulb'+id:t};
const targetOfKey=k=>{const m=String(k).match(/^bulb(\d+)$/);if(m){const b=D.bulbs.find(x=>x.id===+m[1]);if(b)return b.target}return k};
function rebuildRoomMap(){ROOMS={};for(const r of ROOMDATA)for(const d of r.devices)ROOMS[keyOfTarget(d.target)]=r.name}
function applyState(r){
  S.live=!!r.live;LAST=r.devices||{};
  const names=targetsOf('light');
  for(const t of names){if(!BULBID[t])BULBID[t]=nextBulb++;const id=BULBID[t];let b=D.bulbs.find(x=>x.id===id);if(!b){b={id,target:t,hue:38};D.bulbs.push(b)}const st=stateOf(t);
    Object.assign(b,{name:nameOf(t),kind:pluginLabel(t),known:!!st,on:st?!!st.on:false,bri:st?st.bri:0,hue:st?st.hue:b.hue,color:st?!!st.color:false,reason:st?'':reasonOf(t)})}
  D.bulbs=D.bulbs.filter(b=>names.includes(b.target)).sort((a,b)=>a.id-b.id);
  const sockets=targetsOf('plug');
  for(const t of sockets){let p=D.plugs.find(x=>x.target===t);if(!p){p={target:t};D.plugs.push(p)}const s=stateOf(t);Object.assign(p,{name:nameOf(t),known:!!s,on:s?!!s.on:false,reason:s?'':reasonOf(t)})}
  D.plugs=D.plugs.filter(p=>sockets.includes(p.target)).sort((a,b)=>a.name.localeCompare(b.name,undefined,{numeric:true}));
  const coolers=targetsOf('aircon');
  for(const t of coolers){let c=D.acs.find(x=>x.target===t);if(!c){c={target:t};D.acs.push(c)}const s=stateOf(t);Object.assign(c,{name:nameOf(t),known:!!s,on:s?!!s.on:false,mode:s?s.mode:null,temp:s?s.target:null,fan:s?s.fan:null,indoor:s?s.indoor:null,outdoor:s?s.outdoor:null,reason:s?'':reasonOf(t)})}
  D.acs=D.acs.filter(c=>coolers.includes(c.target)).sort((a,b)=>a.name.localeCompare(b.name,undefined,{numeric:true}));
  let t=TGT.speaker,st=stateOf(t);Object.assign(D.speaker,{target:t,name:nameOf(t,'Speaker'),known:!!st,vol:st?st.vol:0,mute:st?!!st.mute:false,night:st&&typeof st.night==='boolean'?st.night:null,source:st?st.source:'',reason:st?'':reasonOf(t)});
  t=TGT.tv;st=stateOf(t);Object.assign(D.tv,{target:t,name:nameOf(t,'TV'),known:!!st,on:st?!!st.on:false,input:st?tvLabel(st.input):'',reason:st?'':reasonOf(t),uncontacted:!!(t&&LAST[t]&&LAST[t].not_contacted)});
  t=TGT.feeder;st=stateOf(t);Object.assign(D.feeder,{target:t,name:nameOf(t,'Feeder'),known:!!st,last:st?st.last:null,portions:st?st.portions:0,reason:st?'':reasonOf(t)});
  t=TGT.litter;st=stateOf(t);Object.assign(D.litter,{target:t,name:nameOf(t,'Litter box'),known:!!st,last:st?st.last:null,busy:st?!!st.busy:false,reason:st?'':reasonOf(t)});
  const p=T.printer;st=stateOf(TGT.printer);p.name=nameOf(TGT.printer,'Printer');
  if(st)Object.assign(p,{known:true,was:true,state:st.state,nozzle:num(st.nozzle),bed:num(st.bed),layer:st.layer,layers:st.layers,pct:num(st.pct)||0,reason:'',lostAt:null});
  else{if(p.known&&!p.lostAt)p.lostAt=Date.now();p.known=false;p.reason=reasonOf(TGT.printer)}
  T.hostName=nameOf(TGT.host,'Host');st=stateOf(TGT.host);if(st){T.hostKnown=true;T.hostWas=true;T.hostUp=!!st.up;T.bootAt=Date.now()-(st.uptime_s||0)*1000}else T.hostKnown=false;
  T.routerName=nameOf(TGT.router,'Router');st=stateOf(TGT.router);T.wgKnown=!!st;T.wgOk=st?!!st.wireguard_up:false;T.wgReason=st?'':reasonOf(TGT.router);
  st=stateOf(TGT.camera);T.camKnown=!!st;if(st)T.camWas=true;T.camOk=st?!!st.streaming:false;T.camReason=st?'':reasonOf(TGT.camera);
  st=stateOf(TGT.telemetry);const c=st&&st.data&&typeof st.data==='object'?st.data:{};
  T.pkgW=num(c.pkg_watts);T.pkgC=num(c.pkg_temp_c);T.cpu=num(c.cpu_usage);T.memUsed=num(c.memory_used_bytes);
  const raw=Array.isArray(c.cores)?c.cores:Array.isArray(c.per_core)?c.per_core:null;
  if(raw){const vals=raw.map(x=>typeof x==='number'?x:(x&&(num(x.usage)!=null?x.usage:num(x.util_pct)))).map(num);const pct=vals.some(v=>v!=null&&v>1);T.cores=vals.length&&vals.every(v=>v!=null)?vals.map(v=>clamp(pct?v/100:v,0,1)):null}else T.cores=null;
  T.memTotal=num(c.host&&c.host.mem_total_bytes);T.guests=guestsOf(c);T.coreKinds=T.cores?kindsOf(c,T.cores.length):null;T.energy=energyOf(c);
  applyMachineStates();rebuildRoomMap();wantApply();
}
function applyMachineStates(){for(const v of VMS){const st=stateOf(v.id);v.known=!!st;v.state=st?st.state:'unknown';v.since=st?Date.now()-(st.uptime_s||0)*1000:0;v.reason=st?'':reasonOf(v.id)}}
async function refreshState(){if(!S.signedIn||S.locked)return;try{const r=await api('GET','/state');applyState(r);render();}catch(e){}}
let soonT=0;function refreshSoon(){clearTimeout(soonT);soonT=setTimeout(refreshState,150)}
async function loadRooms(){const r=await api('GET','/rooms');ROOMDATA=r.rooms||[];ROOM_ORDER=ROOMDATA.map(x=>x.name);ROOM_NAMES=ROOM_ORDER.slice();rebuildRoomMap()}
async function loadMe(){const r=await api('GET','/me');S.session=r.session;PREFS=r.preferences||PREFS;S.assistant=!!r.assistant}
async function loadCatalog(){const r=await api('GET','/catalog');CAT={};for(const a of r.actions||[]){CAT[a.id]=a;for(const old of a.aliases||[])CAT[old]=CAT[old]||a}}
async function loadDevices(){const r=await api('GET','/devices');DEVINFO={};for(const d of r.devices||[])DEVINFO[d.target]=d;pickPrimaries()}
async function loadMachines(){if(!isOwner())return;const r=await api('GET','/machines');HOSTS=r.hosts||[];
  VMS=(r.machines||[]).map(m=>{const d=DEVINFO[m.target];const v=VMS.find(x=>x.id===m.target)||{transition:null,known:false,state:'unknown',since:0,reason:''};const full=(d&&d.name)||m.name||('VM '+m.vmid);
    return Object.assign(v,{id:m.target,key:m.target,vmid:m.vmid,host:m.host,name:full,short:String(full).replace(/ · VM \d+$/,'')||('VM '+m.vmid),icon:'pc',console:hasCap(m.target,'console'),tech:'VM '+m.vmid+(m.host&&m.host!=='primary'?' on '+m.host:''),pending:m.status!=='approved',firstSeen:m.first_seen,approvedAt:m.approved_at,home:m.status==='approved'&&!!d})});
  applyMachineStates()}
/* the electricity price and what each telemetry node's energy has cost so far, which the hub keeps. The owner's */
let ENERGY={known:false,at:0,tariff:null,targets:[]};
/* the hub's tariff, as the page uses it: one price, or a day and a night price with the night hours and the zone they are read in */
function tariffOf(t){if(!t||typeof t!=='object'||typeof t.currency!=='string'||num(t.day_price_per_kwh)==null)return null;const hours=h=>h&&typeof h.start==='string'&&typeof h.end==='string'?{start:h.start,end:h.end}:null;return {currency:t.currency,day:num(t.day_price_per_kwh),night:num(t.night_price_per_kwh),hours:hours(t.night),summer:hours(t.summer_night),zone:typeof t.time_zone==='string'?t.time_zone:null,now:t.band_now==='night'?'night':'day'}}
function readEnergy(r){ENERGY={known:true,at:Date.now(),tariff:tariffOf(r.tariff),targets:Array.isArray(r.targets)?r.targets:[]};return ENERGY}
async function loadEnergy(){if(!isOwner())return ENERGY;try{readEnergy(await api('GET','/energy',undefined,{quiet:true}))}catch(e){ENERGY.at=Date.now()}return ENERGY}
async function saveEnergyTariff(body){return readEnergy(await api('PUT','/energy/tariff',body))}
const energyOfTarget=t=>ENERGY.targets.find(x=>x.target===t)||null;
async function loadHealth(){if(!isOwner())return;try{HEALTHDATA=await api('GET','/health')}catch(e){if(e.status!==401&&e.status!==423)HEALTHDATA=null}}
async function loadPasskeys(){if(!isOwner())return;const r=await api('GET','/passkeys');PASSKEYS=(r.passkeys||[]).filter(k=>!k.revoked_at)}
async function loadUsers(){if(!isOwner())return;const r=await api('GET','/users');USERS=r.users||[];INVITES=r.invitations||[]}
/* the house's scenes: their levels come from the hub, so the page can tell which one is on */
async function loadScenes(){try{const r=await api('GET','/scenes',undefined,{quiet:true});SCENEDEFS=(Array.isArray(r.scenes)?r.scenes:[]).slice().sort((a,b)=>(a.position||0)-(b.position||0))}catch(e){}}
/* the compiled-in plugins and the integrations made of them. Owner only. The list says what each plugin is, what it reaches and whether another can be added; the whole manifest of one (what its setup asks for, the actions it adds) is a request of its own */
const PLUGIN_FULL={};
async function loadPlugins(){if(!isOwner()){PLUGINS={known:false,plugins:[],instances:[]};return PLUGINS}
  try{const r=await api('GET','/plugins',undefined,{quiet:true});
    PLUGINS={known:true,live:!!r.live,plugins:Array.isArray(r.plugins)?r.plugins:[],instances:Array.isArray(r.instances)?r.instances:[]}}
  catch(e){PLUGINS={known:false,why:e.message,plugins:PLUGINS.plugins,instances:PLUGINS.instances}}
  return PLUGINS}
async function loadPluginFull(id){if(PLUGIN_FULL[id])return PLUGIN_FULL[id];try{PLUGIN_FULL[id]=await api('GET','/plugins/'+enc(id),undefined,{quiet:true})}catch(e){PLUGIN_FULL[id]=null}return PLUGIN_FULL[id]}
/* the owner's notes on a device: free lines, such as what goes offline while it is down */
async function loadNotes(t){if(!t||!isOwner())return [];try{const r=await api('GET','/devices/'+enc(t)+'/notes',undefined,{quiet:true});NOTES[t]=Array.isArray(r.notes)?r.notes.map(String):[]}catch(e){if(!NOTES[t])NOTES[t]=[]}return NOTES[t]}
async function saveNotes(t,lines){const r=await api('PUT','/devices/'+enc(t)+'/notes',{notes:lines});NOTES[t]=Array.isArray(r.notes)?r.notes.map(String):lines;return NOTES[t]}
async function loadAudit(){if(!isOwner()||AUDITLOAD.busy)return;AUDITLOAD.busy=true;try{const r=await api('GET','/audit?limit=200');const rows=r.rows||[];const closed=new Set(rows.filter(x=>x.run_id&&x.result!=='commanded').map(x=>x.run_id));
    AUDIT=rows.filter(x=>!(x.result==='commanded'&&x.run_id&&closed.has(x.run_id))).map(x=>{const prior=AUDIT.find(o=>o.id===x.seq);return {id:x.seq,t:new Date(x.ts),action:auditLabel(x.action),rawAction:x.action,target:auditTarget(x.target),channel:x.channel,guard:x.step_up,result:x.result+(x.rtt_ms!=null?' · '+(x.rtt_ms/1000).toFixed(1)+' s':''),who:auditWho(x.user_ref),from:x.device,open:prior?prior.open:false}})}finally{AUDITLOAD.busy=false;AUDITLOAD.at=Date.now()}}
const AUDIT_WORDS={'auth.login':'Sign in','session.lock':'Lock','session.end':'Sign out','passkey.enroll':'First passkey','user.join':'Joined','user.invite_cancel':'Cancel invitation','room.create':'New room','room.rename':'Rename room','room.delete':'Delete room','room.order':'Reorder rooms','room.devices':'Arrange a room','room.scene_save':'Save a room scene','room.scene_delete':'Delete a room scene','device.rename':'Rename','device.notes':'Notes on a device','device.adopt':'Add a device','device.remove':'Remove a device','device.discover':'Look for devices','scene.save':'Change a scene','light.add':'Add a light','light.discover':'Look for lights','light.hue_pair':'Pair a light bridge','light.accept_address':'Accept a new light address','machine.scan':'Look for computers','energy.price':'Set the electricity price','plugin.setup':'Set up an integration','console.connect':'Console connected','console.disconnect':'Console closed','access.owner_only':'Refused: owner only','voice':'Voice','memory.add':'Swifty remembered','memory.forget':'Swifty forgot','learned.forget':'Swifty dropped a learned phrase','swifty.review':'Swifty’s weekly review'};
const auditLabel=a=>(CAT[a]&&CAT[a].label)||AUDIT_WORDS[a]||String(a).replace(/[._]/g,' ');
const auditTarget=t=>{if(!t||t==='-'||t==='new')return '';const d=LAST[t]||DEVINFO[t];if(d&&d.name)return d.name;const r=roomById(t);if(r)return r.name;const v=vmById(t);if(v)return v.name;const k=PASSKEYS.find(p=>p.id===t);if(k)return k.name;return t};
const auditWho=u=>{if(!u||u==='-')return '';if(u===person().id)return person().name;const x=USERS.find(p=>p.id===u);return x?x.name:'Someone'};
async function loadAll(){await Promise.all([loadCatalog(),loadMe()]);if(!isOwner()){VMS=[];HOSTS=[];PASSKEYS=[];USERS=[];INVITES=[];AUDIT=[];HEALTHDATA=null}await Promise.all([loadDevices(),loadRooms(),loadScenes()]);await loadMachines();const st=await api('GET','/state');applyState(st);await Promise.all([loadHealth(),loadPasskeys(),loadUsers(),loadPlugins(),loadEnergy()]);if(PLUGINS.known)applyState({live:S.live,devices:LAST});if(S.view==='rooms')renderRooms();render();S.booted=true}

/* ---------- health: only what the collector says, and never a zero from a source that has gone quiet ---------- */
const FAULTS=[['segv','Crashes','segfaults',['segfault_count_24h']],['gpf','Protection faults','general protection faults',['gpf_count_24h','general_protection_count_24h']],['ill','Bad instructions','invalid opcodes',['invalid_opcode_count_24h','ill_count_24h']],['core','Core dumps','systemd-coredump',['coredump_count_24h','core_dump_count_24h']],['hw','Hardware errors','kernel hardware error',['hardware_error_count_24h','hw_error_count_24h']],['mce','Machine checks','machine-check events',['mce_count_24h','machine_check_count_24h']]];
const faultSection=()=>HEALTHDATA&&HEALTHDATA.faults&&HEALTHDATA.faults.status==='ok'?HEALTHDATA.faults:null;
function faultValue(code){const F=faultSection();if(!F)return null;const f=FAULTS.find(x=>x[0]===code);if(!f)return null;for(const n of f[3]){const c=F.counters&&F.counters[n];if(c&&c.status==='ok'&&num(c.value)!=null)return c.value}return null}
const crashCount=key=>{if(key!=='host')return 0;return ['segv','gpf','ill','hw','mce'].reduce((s,c)=>s+(faultValue(c)||0),0)};
function reporting(key){if(key==='host')return T.hostKnown&&T.hostUp;const v=vmById(key);return !!v&&v.known&&v.state==='running'&&!v.transition&&!v.pending}


function devName(){return (S.session&&S.session.device)||'device'}
function tick(){render()}

export {VMS,HOSTS,PASSKEYS,USERS,INVITES,ROOM_NAMES,ROOM_ORDER,ROOMS,CAT,DEVINFO,LAST,PREFS,HEALTHDATA,AUDIT,SCENEDEFS,PLUGINS,PLUGIN_FULL,NOTES,ENERGY,loadEnergy,saveEnergyTariff,energyOfTarget,setPrefs,AUDITLOAD,vmById,slug,escRe,roomByName,roomById,byTarget,classOf,capsOf,capOf,hasCap,targetsOf,nameOf,TGT,pickPrimaries,pluginOf,pluginLabel,S,person,isOwner,NR,D,T,stateOf,reasonOf,usable,TV_LABEL,tvLabel,tvWire,BULBID,keyOfTarget,targetOfKey,rebuildRoomMap,applyState,refreshState,refreshSoon,loadRooms,loadDevices,loadMachines,loadHealth,loadPasskeys,loadUsers,loadScenes,loadPlugins,loadPluginFull,loadNotes,saveNotes,loadAudit,loadAll,FAULTS,faultSection,faultValue,crashCount,reporting,devName,tick};

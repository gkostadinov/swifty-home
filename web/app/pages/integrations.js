import {$,$$,esc,ICON,PEND} from '../core/util.js';
import {PLUGINS,PLUGIN_FULL,DEVINFO,isOwner,byTarget,nameOf,loadPlugins,loadPluginFull,loadDevices,loadMachines,refreshState} from '../core/store.js';
import {toast} from '../ui/toast.js';
import {ACT} from '../core/catalog.js';
import {armGuard} from '../core/guards.js';
import {onEvent} from '../core/events.js';
import {SHEET,openSheet,row} from '../ui/sheets.js';
import {render} from './home.js';
import {cardOf,cardSheet,iconOf,deviceWord,sentence} from '../cards/index.js';
import {startSetup} from './setup.js';

/* ============================================================ integrations: the plugins this hub was built with, and the integrations made of them */
/* adding, setting up again and removing are catalogue actions (plugin.add, plugin.reconfigure, plugin.remove): each asks for the passkey and leaves an audit row */
const pluginById=id=>PLUGINS.plugins.find(p=>p.id===id)||null;
const pluginName=id=>{const p=pluginById(id);return p?(p.name||p.id):String(id||'')};
const instById=id=>PLUGINS.instances.find(i=>i.id===id)||null;
const devicesOf=id=>Object.keys(DEVINFO).filter(t=>DEVINFO[t].instance===id).sort(byTarget);
/* the hub lists each instance's devices with it: a device waiting for approval (a computer a search found) is not one of the house's yet */
const devicesIn=i=>Array.isArray(i.devices)?i.devices:[];
const deviceCount=i=>devicesIn(i).filter(d=>!d.pending).length;
const waitingCount=i=>devicesIn(i).filter(d=>d.pending).length;
/* an integration is named after its plugin; one of a plugin this build lacks still carries the name the hub kept */
const instName=i=>i.plugin_name||pluginName(i.plugin);
/* where a plugin reaches: only your network, or a vendor's servers, and whether that vendor documents it */
const reachOf=p=>{const r=p&&p.reach;if(!r)return {cloud:false};return r.reach==='cloud'?{cloud:true,vendor:r.vendor||'the vendor',official:r.official!==false}:{cloud:false}};
function badges(p){const r=reachOf(p);return `<span class="badges">${r.cloud?`<span class="badge cloud">${ICON('cloud')}Cloud</span>${r.official?'':'<span class="badge warn">Unofficial</span>'}`:`<span class="badge local">${ICON('rooms')}Local</span>`}</span>`}
const READY={ready:'Ready',needs_setup:'Needs setup',disabled:'Disabled'};
const readinessOf=i=>{const r=i.readiness;if(!r)return {status:'ready',reason:''};if(typeof r==='string')return {status:r,reason:''};return {status:r.status||'ready',reason:r.reason||''}};
const HEALTH={ok:'Healthy',degraded:'Degraded',down:'Down',unknown:'Health not known yet'};
const healthOf=i=>{const h=i.health;if(!h)return {status:'unknown',detail:''};if(typeof h==='string')return {status:h,detail:''};return {status:h.status||'unknown',detail:h.detail||''}};
function instWord(i){const r=readinessOf(i),h=healthOf(i);const n=deviceCount(i),w=waitingCount(i);
  return [i.enabled===false?'Switched off':READY[r.status]||sentence(r.status),r.status==='ready'&&i.enabled!==false?HEALTH[h.status]||sentence(h.status):'',n+(n===1?' device':' devices'),w?w+' waiting for approval':''].filter(Boolean).join(' · ')}
/* what it opens a connection to, as the manifest declares it (the web protocols are matched by shape, so this file names no address scheme) */
const NET={ws:u=>'WebSocket, port '+u.port,tcp:u=>'TCP, port '+u.port,udp:u=>'UDP, port '+u.port,udp_broadcast:u=>'UDP broadcast on your network, port '+u.port,wake_on_lan:()=>'Wake-on-LAN packets on your network',cloud_host:u=>'The internet: '+u.host};
const netWords=u=>{u=u||{};const k=String(u.use||'');if(/^ht{2}ps?$/.test(k))return k.toUpperCase()+', port '+u.port;return (NET[k]||(()=>sentence(k.replace(/_/g,' '))))(u)};
/* a custom action's tier: asked for by its plugin as a word (`tier`), applied by the hub as the console's number (`tier_effective`) */
const TIER={0:'reading',1:'just happens',2:'asks you to confirm',3:'needs your passkey',S:'press and hold'};
const TIER_OF={read:0,comfort:1,consequential:2,critical:3,safe_direction:'S'};
const tierKey=t=>TIER_OF[t]!=null?TIER_OF[t]:t;
const tierWord=t=>{const k=tierKey(t);return TIER[k]!=null?TIER[k]:String(t)};
/* re-render whichever of these pages is open */
const reopen=type=>{if(SHEET.open&&SHEET.type===type)openSheet(type,null,{replace:true,focus:'keep'})};
async function afterChange(){await loadPlugins();await loadDevices();await loadMachines().catch(()=>{});await refreshState();render()}

/* the setup the hub opened for this person: the confirmed run carries it as `setup` (its id, the plugin, the instance, what it is for and the first step) */
const setupFrom=v=>v&&v.setup&&typeof v.setup==='object'&&v.setup.setup_id?v.setup:null;
function openSetupFrom(v){const s=setupFrom(v);if(!s){toast('The hub did not open a setup.','fail');return}startSetup(s.setup_id,s.step,{plugin:s.plugin,instance:s.instance,again:s.purpose==='reconfigure'})}
/* setting an integration up again starts from what it has: its own steps, with the stored values filled in */
function setUpAgain(id,anchor){const i=instById(id);const name=i?instName(i):id;const a=ACT.integration('plugin.reconfigure',id,{},'Set up '+name+' again');
  a.onDone=(rt,v)=>openSetupFrom(v);armGuard(a,anchor,{channel:'touch'})}

const integrationsSheets={
  integrations(){
    if(!isOwner())return {title:'Integrations',html:'<p class="quiet">Only the owner can see the integrations.</p>'};
    const list=PLUGINS.instances.slice().sort((a,b)=>instName(a).localeCompare(instName(b))||byTarget(a.id,b.id));
    const line=i=>{const p=pluginById(i.plugin);const r=readinessOf(i);const warn=i.enabled===false||r.status!=='ready'||healthOf(i).status==='down';
      return `<div class="rowi btnrow intg" data-open="integration:${esc(i.id)}" role="button" tabindex="0">${ICON(warn?'warn':'puzzle')}<div class="t"><b>${esc(instName(i))}${i.id!==i.plugin?` <span class="fine">${esc(i.id)}</span>`:''}</b>${badges(p)}<small>${esc(instWord(i))}</small></div><svg class="ic chev" aria-hidden="true"><use href="#i-chev"/></svg></div>`};
    const html=!PLUGINS.known?`<p class="quiet" id="in-wait">${PLUGINS.why?esc(PLUGINS.why):'The integrations are being read.'}</p>`
      :`<p class="p2">An integration is one use of a plugin: a bridge, a host, a set of speakers. Adding, setting up again and removing each ask for your passkey.</p>
      <div class="acts"><button class="btn primary" data-open="addintegration">${ICON('plus')}Add an integration</button></div>
      ${list.length?`<div class="rows">${list.map(line).join('')}</div>`:'<p class="quiet">No integrations yet.</p>'}`;
    let off=null;
    return {title:'Integrations',html,
      mount(){off=onEvent('plugin',()=>reopen('integrations'));if(!PLUGINS.known)loadPlugins().then(()=>{if(PLUGINS.known)reopen('integrations')})},
      close(){if(off)off()}};
  },
  integration(id){
    const i=instById(id);if(!i)return {title:'Integration',html:'<p class="quiet">That integration is not here any more.</p>'};
    const p=pluginById(i.plugin);const r=readinessOf(i),h=healthOf(i);const reach=reachOf(p);const devs=devicesOf(id);
    const here=i.in_build!==false;
    const html=`<div class="rowi">${ICON('puzzle')}<div class="t"><b>${esc(instName(i))}${i.id!==i.plugin?` <span class="fine">${esc(i.id)}</span>`:''}</b>${badges(p)}<small>${esc(p&&p.summary||'')}</small></div></div>
      <div class="rows">
        ${row(null,'Readiness',esc(i.enabled===false?'This integration is switched off.':r.reason||(r.status==='ready'?'Everything it needs is set.':'')),esc(i.enabled===false?'Off':READY[r.status]||sentence(r.status)))}
        ${row(null,'Health',esc(h.detail||(h.status==='unknown'?'The hub asks it from time to time when it is live.':'')),esc(HEALTH[h.status]||sentence(h.status)))}
        ${i.unavailable?row(null,'Its devices',esc(i.unavailable),null):''}
      </div>
      ${reach.cloud?`<p class="p2">It goes through ${esc(reach.vendor)}’s servers.${reach.official?'':' '+esc(reach.vendor)+' does not document this for others: it can stop working whenever they change something.'}</p>`:''}
      <h3 class="roomhead" style="font-size:17px">Devices</h3>
      ${devs.length?`<div class="rows">${devs.map(t=>row(iconOf(t),esc(nameOf(t)),esc(deviceWord(t)),null,`data-open="${esc(cardSheet(t))}"`)).join('')}</div>`:'<p class="quiet">No devices from this integration yet.</p>'}
      <h3 class="roomhead" style="font-size:17px">Change it</h3>
      ${here?`<div class="acts" id="in-again"><button class="btn primary" data-again>Set up again</button></div>
      <p class="p2">Runs its setup again, starting from what it has: after a new pairing, a changed address or a new token.</p>`:`<p class="p2">This build of Swifty Home does not have the plugin this was made with, so it cannot be set up again. It can still be removed.</p>`}
      <div class="acts" id="in-remove"><button class="btn danger" data-remove>Remove this integration</button></div>
      <p class="p2">The hub lists everything that goes with it first: its devices, their places in rooms, favourites and its stored secrets. The history keeps every row.</p>`;
    let off=null;
    return {title:instName(i),html,
      mount(root){off=onEvent('plugin',d=>{if(!d||!d.instance||d.instance===id){if(!$('[data-guard]',root))reopen('integration:'+id)}});
        const again=$('[data-again]',root);if(again)again.onclick=()=>setUpAgain(id,$('#in-again',root));
        $('[data-remove]',root).onclick=()=>{const a=ACT.integration('plugin.remove',id,{},'Remove '+instName(i)+(i.id!==i.plugin?' ('+i.id+')':''));a.holdLabel='Hold to remove';a.doneText='Removed';
          a.onDone=async()=>{await afterChange();toast(instName(i)+' is removed.','ok');if(SHEET.open&&SHEET.type==='integration:'+id){SHEET.stack=[];openSheet('integrations',null,{replace:true})}};
          armGuard(a,$('#in-remove',root),{channel:'touch'})}},
      close(){if(off)off()}};
  },
  /* the plugins this hub was built with: what each is, where it reaches, then its own setup */
  addintegration(){
    const ps=PLUGINS.plugins.slice().sort((a,b)=>pluginName(a.id).localeCompare(pluginName(b.id)));
    const used=id=>PLUGINS.instances.some(i=>i.plugin===id);
    const html=`<p class="p2">Pick what to add. Before anything else you see what it reaches, on your network or on the internet.</p>
      ${ps.length?`<div class="rows">${ps.map(p=>`<div class="rowi btnrow" data-open="plugin:${esc(p.id)}" role="button" tabindex="0">${ICON('puzzle')}<div class="t"><b>${esc(p.name||p.id)}</b>${badges(p)}<small>${esc(p.summary||'')}${used(p.id)?esc(p.multi_instance?' · Added already; another can be added':' · Added already'):''}</small></div><svg class="ic chev" aria-hidden="true"><use href="#i-chev"/></svg></div>`).join('')}</div>`:'<p class="quiet">This hub lists no plugins.</p>'}`;
    return {title:'Add an integration',html};
  },
  plugin(id){
    const p=pluginById(id);if(!p)return {title:'Plugin',html:'<p class="quiet">This hub was not built with that plugin.</p>'};
    /* what its setup asks for and the actions it adds are in its own manifest, read when its page is opened */
    const full=PLUGIN_FULL[id];const reach=reachOf(p);const taken=PLUGINS.instances.filter(i=>i.plugin===id);
    const fields=full?(full.config||[]).map(f=>f.label||f.key).concat((full.secrets||[]).map(s=>(s.label||s.slot)+' (kept secret)')):[];
    const acts=full?full.actions||[]:[];
    const html=`<div class="rowi">${ICON('puzzle')}<div class="t"><b>${esc(p.name||p.id)} <span class="fine">${esc(p.version||'')}</span></b>${badges(p)}<small>${esc(p.summary||'')}</small></div></div>
      ${reach.cloud&&!reach.official?`<div class="confirm warnbox"><p class="sent">${ICON('warn')}Not an official integration</p><p class="p2">${esc(reach.vendor)} does not open its servers to others. This plugin talks to them the way ${esc(reach.vendor)}’s own app does. That may be against their terms, and it can stop working whenever they change something. Its setup starts by saying so.</p></div>`:''}
      <h3 class="roomhead" style="font-size:17px">What it reaches</h3>
      <div class="rows">${reach.cloud?row('cloud','Through '+esc(reach.vendor)+'’s servers','Your devices are reached over the internet.',null):row('rooms','Only your network','It talks to devices at home and nothing else.',null)}${(p.network||[]).map(u=>row(null,esc(netWords(u)),null,null)).join('')}</div>
      ${(p.classes||[]).length?`<h3 class="roomhead" style="font-size:17px">What it brings</h3><p class="p2">${esc((p.classes||[]).map(c=>cardOf(c).label).filter((x,i,all)=>all.indexOf(x)===i).join(', '))}.</p>`:''}
      ${fields.length?`<h3 class="roomhead" style="font-size:17px">What its setup asks for</h3><p class="p2">${esc(fields.join(', '))}.</p>`:''}
      ${acts.length?`<h3 class="roomhead" style="font-size:17px">Actions of its own</h3><div class="rows">${acts.map(a=>{const asked=tierKey(a.tier),got=a.tier_effective!=null?a.tier_effective:asked;return row(null,esc(a.label||a.id),esc(a.description||''),esc(got!==asked?tierWord(got)+' (asked for: '+tierWord(asked)+')':tierWord(got)))}).join('')}</div>`:''}
      ${full===undefined?`<p class="quiet">${PEND} Reading the rest of what it says about itself…</p>`:''}
      ${p.can_add===false?`<p class="quiet">${esc(p.why_not||'It cannot be added now.')}</p>${taken.length?`<div class="acts"><button class="btn" data-open="integration:${esc(taken[0].id)}">Open ${esc(instName(taken[0]))}</button></div>`:''}`
        :`<h3 class="roomhead" style="font-size:17px">Set it up</h3>
      ${taken.length?`<input class="field" id="pl-inst" placeholder="An id for this one (optional)" aria-label="Integration id" maxlength="16" autocomplete="off" autocapitalize="off" spellcheck="false"><p class="p2">There is one already. Left empty, this one gets the next free id. Or choose one: a lower-case letter, then up to 15 lower-case letters and digits.</p>`:''}
      <div class="acts" id="pl-acts"><button class="btn primary" data-add>Set up ${esc(p.name||p.id)}</button></div>
      <p class="p2">The setup is the plugin’s own: a few steps, here on this page. Your passkey starts it.</p>`}`;
    return {title:p.name||p.id,html,mount(root){
      if(full===undefined)loadPluginFull(id).then(()=>reopen('plugin:'+id));
      const b=$('[data-add]',root);if(!b)return;b.onclick=()=>{const params={plugin:id};const f=$('#pl-inst',root);
        if(f&&f.value.trim()){const v=f.value.trim();if(!/^[a-z][a-z0-9]{0,15}$/.test(v)||v==='new'||PLUGINS.instances.some(i=>i.id===v)){toast('That id is taken or not allowed. Use a lower-case letter, then up to 15 lower-case letters and digits.','');f.focus();return}params.instance=v}
        const a=ACT.integration('plugin.add','new',params,'Add '+(p.name||p.id));a.onDone=(rt,v)=>openSetupFrom(v);armGuard(a,$('#pl-acts',root),{channel:'touch'})}}};
  },
};

export {pluginName,reachOf,afterChange,setUpAgain,integrationsSheets};

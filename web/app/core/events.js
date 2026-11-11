import {apiUrl} from './api.js';
import {S,LAST,isOwner,applyState,refreshState,refreshSoon,loadDevices,loadMachines,loadScenes,loadPlugins,loadAll} from './store.js';
import {render} from '../pages/home.js';
import {refreshSheet} from '../ui/sheets.js';

/* ============================================================ events: the hub says when something changed, so the page asks again sooner */
/* the stream only nudges: every reading still comes from the API, and the page's own polling stays for when the stream is down */
const EV={es:null,retryT:0,devT:0,listeners:{},runs:new Map()};
/* a page that cares about one kind of event: the setup wizard for `setup`, the integrations list for `plugin` */
function onEvent(kind,fn){(EV.listeners[kind]=EV.listeners[kind]||new Set()).add(fn);return ()=>EV.listeners[kind].delete(fn)}
function emit(kind,data){for(const f of EV.listeners[kind]||[]){try{f(data)}catch(e){}}}
const data=e=>{try{return JSON.parse(e.data||'{}')}catch(err){return {}}};
/* following a run waits for its next event, or for the poll interval, whichever comes first */
function runWait(id,ms){return new Promise(res=>{const t=setTimeout(done,ms);function done(){clearTimeout(t);const w=EV.runs.get(id);if(w){w.delete(done);if(!w.size)EV.runs.delete(id)}res()}if(id){if(!EV.runs.has(id))EV.runs.set(id,new Set());EV.runs.get(id).add(done)}})}
/* the device list changed: names, classes, what is there at all; the hub says the same when the house's scenes were saved, so their levels are read again too. Several at once are one refetch */
function devicesChanged(){clearTimeout(EV.devT);EV.devT=setTimeout(async()=>{if(!S.signedIn||S.locked)return;try{await loadDevices();await loadMachines();await loadScenes();await refreshState();emit('devices',{});refreshSheet()}catch(e){}},250)}
function startEvents(){
  if(EV.es||!S.signedIn||S.locked||typeof EventSource==='undefined')return;clearTimeout(EV.retryT);
  const es=new EventSource(apiUrl('/events'));EV.es=es;
  es.addEventListener('run',e=>{const d=data(e);const w=d.run_id&&EV.runs.get(d.run_id);if(w)[...w].forEach(f=>f());emit('run',d);refreshSoon()});
  /* a reading that differs from the last one: laid over what the page has, and drawn at once */
  es.addEventListener('state',e=>{const d=data(e);const cur=d.target&&LAST[d.target];if(cur&&d.state&&typeof d.state==='object'){cur.available=true;cur.state=d.state;delete cur.error;delete cur.simulated;applyState({live:S.live,devices:LAST});render()}else refreshSoon()});
  es.addEventListener('devices',devicesChanged);
  es.addEventListener('plugin',e=>{const d=data(e);if(isOwner())loadPlugins().then(()=>emit('plugin',d));devicesChanged()});
  es.addEventListener('setup',e=>emit('setup',data(e)));
  es.addEventListener('assistant',e=>emit('assistant',data(e)));
  /* this page missed too much: everything is asked for again */
  es.addEventListener('resync',()=>{loadAll().then(()=>{emit('devices',{});refreshSheet()}).catch(()=>{})});
  /* the browser reconnects by itself after a dropped line; a refused stream (signed out, locked) is tried again later */
  es.onerror=()=>{if(es.readyState===2){if(EV.es===es)EV.es=null;clearTimeout(EV.retryT);EV.retryT=setTimeout(startEvents,15000)}};
}
function stopEvents(){clearTimeout(EV.retryT);clearTimeout(EV.devT);if(EV.es){EV.es.close();EV.es=null}for(const w of EV.runs.values())[...w].forEach(f=>f());}
const eventsOn=()=>!!EV.es&&EV.es.readyState===1;

export {onEvent,runWait,startEvents,stopEvents,eventsOn};

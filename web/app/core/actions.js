import {nid} from './util.js';
import {api,enc} from './api.js';
import {LAST,slug,S,D,applyState,refreshSoon} from './store.js';
import {bulbById,roomBulbs,render} from '../pages/home.js';
import {toast} from '../ui/toast.js';
import {activeScene,activeRoomScene} from '../ui/scenes.js';
import {SHEET,refreshSheet} from '../ui/sheets.js';
import {runWait} from './events.js';

/* ============================================================ running an action: nothing is confirmed before the device answers */
const PENDING={},ACTIVE={};let NORESP=[];const LASTDONE={};
/* the benefit of the doubt: what someone just asked for is shown in full from the tap, through the command, and until the device's own reading agrees or WANT_MS has passed, so a poll or a redraw cannot take it back. A failure lets go at once, so the real value and the failure show. Machines, the printer, pets and passkeys have no lean, because there the honest state matters more than the feel */
const WANT_MS=10000,PRESSED={};let WANTED=[];
const sceneHolder=room=>{const k=room?'room-'+slug(room):'lights';return PRESSED[k]||(PRESSED[k]={known:true,room:room||null,scene:null})};
const sceneNow=h=>h.room?activeRoomScene(h.room):activeScene();
const wantItems=a=>{const m=String(a.key).match(/^bulb(\d+)$/);if(m){const b=bulbById(+m[1]);return b?[b]:[]}if(a.key==='lights')return D.bulbs.slice();if(a.key==='speaker')return [D.speaker];if(a.key==='tv')return [D.tv];if(a.room)return roomBulbs(a.room);return D.plugs.concat(D.acs).filter(p=>p.target===a.key)};
const wantMet=(o,f,v)=>{if(f==='on')return !!o.on===!!v;if(f==='bri')return Math.abs(o.bri-v)<=1||(v<10&&o.bri===10);if(f==='hue'){const d=Math.abs(o.hue-v);return Math.min(d,360-d)<=2}return o[f]===v};
function wantPaint(){for(const w of WANTED)for(const o of w.items)for(const [f,v] of Object.entries(w.fields))o[f]=f==='on'?!!v:v}
function wantSet(a){if(!a.lean)return;const fields=Object.assign({},a.lean);const scene=fields.scene;delete fields.scene;
  /* a newer action on the same key replaces the older one, field by field. A scene and a level cannot both stand */
  for(const w of WANTED)if(w.a.key===a.key)for(const f of Object.keys(w.fields))if(f in fields||f==='scene'||scene)delete w.fields[f];
  WANTED=WANTED.filter(w=>Object.keys(w.fields).length);
  if(Object.keys(fields).length)WANTED.push({a,fields,items:wantItems(a),until:0});
  if(scene)WANTED.push({a,fields:{scene},items:[sceneHolder(a.room||null)],until:0});
  wantPaint()}
function wantSettled(a){for(const w of WANTED)if(w.a===a)w.until=Date.now()+WANT_MS}
function wantClear(a){if(!WANTED.some(w=>w.a===a))return;WANTED=WANTED.filter(w=>w.a!==a);applyState({live:S.live,devices:LAST})}
/* runs on fresh state. A device that is not reporting cannot agree, and is no reason to keep waiting */
function wantApply(){const now=Date.now();for(const k in PRESSED)PRESSED[k].scene=sceneNow(PRESSED[k]);
  WANTED=WANTED.filter(w=>!(w.until&&(now>=w.until||w.items.every(o=>!o.known||Object.entries(w.fields).every(([f,v])=>wantMet(o,f,v))))));
  wantPaint()}
const scenePressed=room=>{const h=sceneHolder(room);return WANTED.some(w=>w.items.includes(h))?h.scene:sceneNow(h)};
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
let reqSeq=0;const reqId=()=>'p'+Date.now().toString(36)+'-'+(++reqSeq);
const clientChannel=ch=>ch==='typed'?'palette':'panel';
/* a run is asked about again when its event arrives, or after the usual pause when the stream is down */
async function followRun(v){const t0=Date.now();while(v&&v.phase==='commanded'&&v.run_id){if(Date.now()-t0>240000)break;await runWait(v.run_id,600);v=await api('GET','/actions/runs/'+enc(v.run_id))}return v}
async function run(a){
  a.id=a.id||nid();a.t0=performance.now();ACTIVE[a.id]=a;wantSet(a);PENDING[a.key]=a;render();
  let v;
  try{
    if(a.local){await a.local();v={phase:'confirmed'}}
    else if(a.prepared){const p=a.prepared;const body={prepared_id:p.prepared_id,action:p.action,target:p.target,params:p.params};if(p.needs==='passkey')body.assertion=a.assertion;else body.confirmation=p.confirmation;a.prepared=null;a.assertion=null;v=await api('POST','/actions/confirm',body)}
    else v=await api('POST','/actions/execute',{action:a.action,target:a.target,params:a.params||{},channel:clientChannel(a.channel),request_id:reqId()});
    if(v.phase==='commanded'&&a.onAccept){a.onAccept();render()}
    v=await followRun(v);
  }catch(e){refused(a,e);return}
  a.result=v;
  if(v.phase==='confirmed'||v.phase==='simulated'||v.phase==='sent')settled(a,v);else failAction(a,v);
}
let simToastAt=0;
function settled(a,v){
  const rt=((performance.now()-a.t0)/1000).toFixed(1);const sim=v.phase==='simulated';const note=!sim&&v.detail&&(v.steps||[]).some(s=>s.phase==='no_response')?String(v.detail):'';wantSettled(a);
  delete PENDING[a.key];const hadBand=NORESP.some(n=>n.key===a.key);NORESP=NORESP.filter(n=>n.key!==a.key);
  LASTDONE[a.key]={text:sim?'Dry-run. Nothing was sent.':((a.reachedWord?a.reachedWord[0].toUpperCase()+a.reachedWord.slice(1)+' · '+rt+' s':a.doneText)||'Done')+(note?' · '+note:''),t:Date.now()};
  a.clear&&a.clear();delete ACTIVE[a.id];
  a.onDone&&a.onDone(rt,v);
  if(sim&&Date.now()-simToastAt>3000){simToastAt=Date.now();toast(a.label+': dry-run, nothing was sent.','')}
  else if(!sim&&a.reachedWord)toast(a.label+': '+a.reachedWord+' after '+rt+' s','ok');
  if(note&&a.level==='plain')toast(note,'');
  refreshSoon();render();if(hadBand)refreshSheet();
}
function failAction(a,v){
  delete PENDING[a.key];wantClear(a);a.clear&&a.clear();const dev=a.deviceName||a.targetName;const detail=v&&v.detail?String(v.detail):'';
  const text=v&&v.phase==='no_response'?`The ${dev} didn't answer.${a.stillText?' Still '+a.stillText+'.':''}${detail?' '+detail:''}`:detail||`The ${dev} refused that.`;
  NORESP=NORESP.filter(n=>n.key!==a.key);NORESP.push({key:a.key,dev,id:a.id,text});
  if(a.onFail)a.onFail(text);
  else if(SHEET.open)refreshSheet();
  else toast(text,'fail',a.level==='plain'?[{l:'Retry',fn:()=>retry(a.id)},{l:'Dismiss',fn:()=>dismissFail(a.id)}]:[{l:'Dismiss',fn:()=>dismissFail(a.id)}]);
  refreshSoon();render();
}
/* the server said no before anything was sent: an answer, not a silent device */
function refused(a,e){delete PENDING[a.key];wantClear(a);a.clear&&a.clear();delete ACTIVE[a.id];if(a.onFail)a.onFail(e.message);else toast(e.message,'fail');render()}
function retry(id){const a=ACTIVE[id];if(!a)return;NORESP=NORESP.filter(n=>n.id!==id);if(a.level==='plain'||a.local)run(a);else{delete ACTIVE[id];toast('Do that again from its page: it needs a fresh confirmation.','')}}
function dismissFail(id){NORESP=NORESP.filter(n=>n.id!==id);const a=ACTIVE[id];if(a)delete ACTIVE[id];render();refreshSheet()}

export {PENDING,NORESP,LASTDONE,wantApply,scenePressed,sleep,clientChannel,run,refused,retry,dismissFail};

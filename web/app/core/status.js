import {isPhone,DAYS,MONTHS} from './util.js';
import {VMS,vmById,S,person,isOwner,D,T,TGT,usable,crashCount,reporting,devName} from './store.js';
import {PENDING,NORESP} from './actions.js';

/* ============================================================ status: one sentence, and only what is known */
const machineName=key=>key==='host'?T.hostName:((vmById(key)||{}).name||'a computer');
const notReporting=()=>{const a=[];for(const b of D.bulbs)if(!b.known)a.push(b.name);for(const k of [D.speaker,D.tv])if(k.target&&!k.known&&usable(k.target))a.push(k.name);return a};
function problems(){
  const list=[];const host=T.hostName;
  if(isOwner()&&TGT.host&&T.hostKnown&&!T.hostUp&&!PENDING.host)list.push({key:'host',name:host,sheet:'host',text:`${host} is off.`,hint:'Someone at home has to press its button. Nothing on it can be reached until then.',short:'off'});
  if(isOwner()&&TGT.host&&S.live&&T.hostWas&&!T.hostKnown&&!PENDING.host)list.push({key:'hostgone',name:host,sheet:'host',text:`${host} stopped answering.`,hint:`Tap ${host} to see what it last reported.`,short:'no answer'});
  if(S.live&&isOwner()&&TGT.printer&&T.printer.was&&!T.printer.known)list.push({key:'printer',name:'Printer',sheet:'printer',text:`The ${T.printer.name} stopped responding.`,hint:'Tap Printer to see what it last reported.',short:'not responding'});
  if(S.live&&TGT.camera&&T.camKnown&&!T.camOk)list.push({key:'camera',name:'Camera',sheet:'camera',text:'The camera is not answering.',hint:cameraOnHome()?'Tap Camera to see more.':'Open Camera from More to see more.',short:'no answer'});
  if(isOwner()&&TGT.host){const n=reporting('host')?crashCount('host'):0;if(n)list.push({key:'crash',name:host,sheet:'host',text:`${host} logged ${n} crash${n===1?'':'es'} today.`,hint:`Tap ${host} to see ${n===1?'it':'them'}.`,short:'crashes'})}
  return list;
}
/* a restart in progress is the most current thing to say, so it outranks the problem list */
function transitions(){
  if(PENDING.host){const stop=PENDING.host.progressText==='shutting down';return {text:stop?`${T.hostName} is shutting down.`:`${T.hostName} is restarting.`,hint:stop?'Everything on it stops. It cannot be powered on from here.':'Everything is back in about 3 minutes.'}}
  if(!isOwner())return null;
  const v=VMS.find(x=>x.transition);if(!v)return null;
  return {text:`The ${v.name} is ${v.transition}.`,hint:'This takes a moment.'};
}
function status(){
  if(!S.booted)return {text:'Swifty Home',hint:''};
  const tr=transitions();if(tr)return tr;
  const p=problems();
  if(p.length)return {text:p[0].text,hint:p[0].hint,problem:p[0]};
  if(NORESP.length){const n=NORESP[NORESP.length-1];return {text:`The ${n.dev} didn't answer.`,hint:'Nothing changed. Try again when you like.',noresp:n}}
  const d=new Date();const when=`${DAYS[d.getDay()]} ${d.getDate()} ${MONTHS[d.getMonth()]} · ${person().name}, signed in with ${devName()}`;
  if(!S.live)return {text:'Dry-run. No device is contacted.',hint:when};
  const quiet=notReporting();
  if(quiet.length)return {text:quiet.length===1?`${quiet[0]} is not reporting.`:`${quiet.length} devices are not reporting.`,hint:when};
  return {text:'All is well at home.',hint:when};
}
/* the owner's phone keeps Camera under More so the first screen stays within its budget; everyone else sees it on Home */
const cameraOnHome=()=>!(isOwner()&&isPhone());

export {machineName,problems,status,cameraOnHome};

import {$,$$,esc,ICON} from '../core/util.js';
import {DEVINFO,LAST,NOTES,TGT,classOf,nameOf,byTarget,isOwner,NR,stateOf,reasonOf,usable,keyOfTarget,loadNotes,saveNotes} from '../core/store.js';
import {toast} from '../ui/toast.js';
import {row,roomOf} from '../ui/sheets.js';

/* ============================================================ cards: a device's class picks its card, and its capabilities pick the controls on it */
/* nothing here knows a target, a vendor or a host: a class names its card, and the card's page takes the device's target */
const CARDS={
  light:{label:'Lights',one:'Light',icon:'bulb',sheet:()=>'lights'},
  bridge:{label:'Bridges',one:'Bridge',icon:'tech',sheet:t=>'service:'+t},
  plug:{label:'Sockets',one:'Socket',icon:'usb',sheet:t=>'plug:'+t},
  aircon:{label:'Air conditioning',one:'Air conditioner',icon:'snow',sheet:t=>'ac:'+t},
  speaker:{label:'Speakers',one:'Speaker',icon:'speaker',sheet:t=>t===TGT.speaker?'music':'device:'+t},
  tv:{label:'TVs',one:'TV',icon:'tv',sheet:t=>t===TGT.tv?'music':'device:'+t},
  feeder:{label:'Feeders',one:'Feeder',icon:'paw',sheet:t=>t===TGT.feeder?'pets':'device:'+t},
  litter:{label:'Litter boxes',one:'Litter box',icon:'paw',sheet:t=>t===TGT.litter?'pets':'device:'+t},
  printer:{label:'Printers',one:'Printer',icon:'printer',sheet:t=>t===TGT.printer?'printer':'device:'+t},
  vm:{label:'Computers',one:'Computer',icon:'pc',sheet:t=>'comp:'+t},
  host:{label:'Hosts',one:'Host',icon:'tech',sheet:t=>'host:'+t},
  router:{label:'Routers',one:'Router',icon:'tech',sheet:t=>'router:'+t},
  camera:{label:'Cameras',one:'Camera',icon:'camera',sheet:t=>t===TGT.camera?'camera':'service:'+t},
  service:{label:'Services',one:'Service',icon:'tech',sheet:t=>'service:'+t},
  telemetry:{label:'Readings',one:'Telemetry',icon:'tech',sheet:t=>'telemetry:'+t},
};
/* any class this page has no card for, today's `other` and whatever a later plugin brings */
const GENERIC={label:'Other devices',one:'Device',icon:'tech',sheet:t=>'device:'+t};
const cardOf=cls=>CARDS[cls]||GENERIC;
const hasCard=cls=>Object.prototype.hasOwnProperty.call(CARDS,cls);
const cardSheet=t=>cardOf(classOf(t)).sheet(t);
const iconOf=t=>cardOf(classOf(t)).icon;

/* ---------- what a device says, in a few words, whatever its class ---------- */
const sentence=v=>{const s=String(v);return s.charAt(0).toUpperCase()+s.slice(1)};
const fieldName=k=>sentence(String(k).replace(/_/g,' '));
const scalar=v=>v===null||['string','number','boolean'].includes(typeof v);
const shown=v=>typeof v==='boolean'?(v?'Yes':'No'):typeof v==='number'?String(Math.round(v*100)/100):String(v);
/* the readings of a state, as label and value: a custom state's fields, or every plain value of a known one */
function readings(st){if(!st)return [];const src=st.kind==='custom'?(st.fields||{}):st.kind==='telemetry'?(st.data||{}):st;
  return Object.entries(src).filter(([k,v])=>k!=='kind'&&scalar(v)).map(([k,v])=>[fieldName(k),v==null?'—':shown(v)])}
function deviceWord(t){
  if(!usable(t))return (DEVINFO[t]||{}).reason||'Not available';const st=stateOf(t);if(!st)return reasonOf(t)||NR;
  switch(st.kind){
    case 'light':return st.on?'On at '+st.bri+'%':'Off';
    case 'plug':case 'tv':return st.on?'On':'Off';
    case 'aircon':return st.on?'On'+(st.target!=null?' at '+st.target+'°':''):'Off';
    case 'speaker':return st.mute?'Muted':'Volume '+st.vol;
    case 'feeder':return st.portions+' portions today';
    case 'litter':return st.busy?'Cleaning':'Idle';
    case 'printer':return sentence(st.state);
    case 'vm':return st.state==='running'?'On':'Off';
    case 'host':return st.up?'Up':'Down';
    case 'router':return st.wireguard_up?'Tunnel up':'Tunnel down';
    case 'camera':return st.streaming?'Answering':'Not answering';
    case 'bridge':case 'service':return st.reachable?'Answering':'Not answering';
    case 'telemetry':return 'Reporting';
    default:{const r=readings(st);return r.length?r.slice(0,2).map(([k,v])=>k+' '+v).join(' · '):'Reporting'}
  }
}

/* ---------- the owner's notes: shown on the device's page, and wherever an action on it asks to be confirmed ---------- */
const MAX_NOTES=8,MAX_NOTE=200;
function notesInner(t){const n=NOTES[t];
  if(n===undefined)return '<p class="quiet">Loading…</p>';
  return `${n.length?`<ul class="notelist">${n.map(l=>`<li>${ICON('pencil')}<span>${esc(l)}</span></li>`).join('')}</ul>`:'<p class="quiet">No notes yet. A note shows wherever an action on this device asks you to confirm.</p>'}
    <div class="acts" data-notes-acts><button class="pill" data-notes-edit>${ICON('pencil')}${n.length?'Edit notes':'Add a note'}</button></div>
    <div data-notes-editor hidden><textarea class="field notesed" rows="4" aria-label="Notes, one per line" spellcheck="true">${esc(n.join('\n'))}</textarea><p class="p2">One note per line, at most ${MAX_NOTES}. Only you see them.</p><div class="acts"><button class="btn primary" data-notes-save>Save</button><button class="btn" data-notes-cancel>Cancel</button></div></div>`}
/* a page about two devices names whose notes each block holds */
const notesBlock=(t,whose)=>!isOwner()||!t?'':`<h3 class="roomhead" style="font-size:17px">${whose?'Notes on the '+esc(whose):'Notes'}</h3><div class="notes" data-notes="${esc(t)}">${notesInner(t)}</div>`;
function wireNotes(root){
  $$('[data-notes]',root).forEach(box=>{const t=box.dataset.notes;
    const paint=()=>{if(!box.isConnected)return;box.innerHTML=notesInner(t);wire()};
    const wire=()=>{const ed=$('[data-notes-editor]',box),ac=$('[data-notes-acts]',box);const edit=$('[data-notes-edit]',box);if(!edit)return;
      edit.onclick=()=>{ed.hidden=false;ac.hidden=true;const ta=$('textarea',ed);ta.focus()};
      $('[data-notes-cancel]',box).onclick=paint;
      $('[data-notes-save]',box).onclick=async e=>{const b=e.currentTarget;const lines=$('textarea',ed).value.split('\n').map(l=>l.trim()).filter(Boolean);
        if(lines.length>MAX_NOTES){toast('At most '+MAX_NOTES+' notes.','');return}if(lines.some(l=>l.length>MAX_NOTE)){toast('A note can be at most '+MAX_NOTE+' characters.','');return}
        b.disabled=true;try{await saveNotes(t,lines);toast('Notes saved.','ok');paint()}catch(err){b.disabled=false;toast(err.message,'fail')}}};
    if(NOTES[t]===undefined)loadNotes(t).then(paint);else wire()});
}

/* ---------- every device, by kind: the way to any device's page, including those Home does not show ---------- */
const cardsSheets={
  devices(){
    const groups={};for(const t of Object.keys(DEVINFO).sort(byTarget)){const c=classOf(t);const k=hasCard(c)?c:'';(groups[k]=groups[k]||[]).push(t)}
    const order=Object.keys(CARDS).concat(['']);
    const line=t=>{const room=roomOf(keyOfTarget(t));return row(iconOf(t),esc(nameOf(t)),esc(deviceWord(t)+(room?' · '+room:'')),null,`data-open="${esc(cardSheet(t))}"`)};
    const html=order.filter(k=>groups[k]).map(k=>`<h3 class="roomhead" style="font-size:17px">${esc(cardOf(k||null).label)}</h3><div class="rows">${groups[k].sort((a,b)=>nameOf(a).localeCompare(nameOf(b),undefined,{numeric:true})).map(line).join('')}</div>`).join('')||'<p class="quiet">No devices yet.'+(isOwner()?' Add an integration to bring some in.':'')+'</p>';
    return {title:'Devices',html:`<p class="p2">Every device you can use, by kind. Tap one for its page.</p>${html}`};
  },
};

export {CARDS,GENERIC,cardOf,hasCard,cardSheet,iconOf,sentence,fieldName,readings,deviceWord,notesBlock,wireNotes,cardsSheets};

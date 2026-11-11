import {$,$$,esc,ICON,PEND} from '../core/util.js';
import {api,enc} from '../core/api.js';
import {VMS,ROOM_ORDER,ROOMS,DEVINFO,vmById,slug,roomByName,person,isOwner,NR,D,classOf,nameOf,byTarget,targetOfKey,rebuildRoomMap,loadRooms} from '../core/store.js';
import {lean} from '../ui/roomlight.js';
import {bulbById,bulbName,roomBulbs,lightsSummary,pinnedRoom,render} from './home.js';
import {PENDING,scenePressed} from '../core/actions.js';
import {toast} from '../ui/toast.js';
import {ACT} from '../core/catalog.js';
import {roomSceneDefs,runRoomScene} from '../ui/scenes.js';
import {sliderEl,wireSlider} from '../ui/gestures.js';
import {dispatch} from '../core/guards.js';
import {SHEET,openSheet,sheetBack,row,roomOf,failHtml} from '../ui/sheets.js';
import {CARDS,iconOf} from '../cards/index.js';

/* what a device is called while it is being moved, the row on its page that moves it, and everything that can move besides the lights */
const placeName=key=>{const v=vmById(key);if(v)return v.name;return nameOf(targetOfKey(key),key)};
const placeRow=(key,label)=>isOwner()&&key?`<div class="rows">${row('rooms',esc(roomOf(key)||'No room'),esc(label||'Room'),null,`data-open="where:${esc(key)}"`)}</div>`:'';
const deviceCount=r=>{const n=Object.values(ROOMS).filter(x=>x===r).length;return n+' device'+(n===1?'':'s')};
/* every device that is not a light, sockets and air conditioners first, then each class in the order of its card */
const classRank=t=>{const i=Object.keys(CARDS).indexOf(classOf(t));return i<0?99:i};
const placeable=()=>{const first=[...D.plugs.map(p=>[p.target,'usb']),...D.acs.map(c=>[c.target,'snow']),...VMS.filter(v=>v.home).map(v=>[v.id,v.icon])];const seen=new Set(first.map(x=>x[0]));
  return first.concat(Object.keys(DEVINFO).filter(t=>!seen.has(t)&&classOf(t)!=='light'&&classOf(t)!=='vm').sort((a,b)=>classRank(a)-classRank(b)||byTarget(a,b)).map(t=>[t,iconOf(t)]))};
const roomsSheets={
  arrange(){
    const html=`<p class="p2">Rooms in the order they appear. Everything can move between rooms.</p>
      ${ROOM_ORDER.length?`<div class="rows">${ROOM_ORDER.map((r,i)=>`<div class="rowi">${ICON('rooms')}<div class="t"><b>${esc(r)}</b><small>${deviceCount(r)}</small></div><button class="pill" data-mv="${i}:-1" aria-label="Move ${esc(r)} up" ${i===0?'disabled':''}>${ICON('up')}</button><button class="pill" data-mv="${i}:1" aria-label="Move ${esc(r)} down" ${i===ROOM_ORDER.length-1?'disabled':''}>${ICON('down')}</button><button class="pill" data-ren="${i}" aria-label="Rename ${esc(r)}">${ICON('pencil')}</button><button class="pill" data-del="${i}" aria-label="Delete ${esc(r)}">${ICON('x')}</button></div>`).join('')}</div>`:'<p class="quiet">No rooms yet.</p>'}
      <div class="inline-edit"><input class="field" id="ar-new" placeholder="A new room" aria-label="New room name" maxlength="40" autocomplete="off"><button class="btn primary" id="ar-add">Add</button></div>
      <h3 class="roomhead" style="font-size:17px">Lights</h3>
      <div class="rows">${D.bulbs.map(b=>`<div class="rowi">${ICON('bulb')}<div class="t"><b>${esc(bulbName(b))} <span class="fine">${esc(b.kind)}</span></b><small>${esc(roomOf('bulb'+b.id)||'No room yet')}</small></div><button class="pill" data-room-pick="bulb${b.id}">Move</button></div>`).join('')}</div>
      <h3 class="roomhead" style="font-size:17px">Everything else</h3>
      <div class="rows">${placeable().map(([key,icon])=>`<div class="rowi">${ICON(icon)}<div class="t"><b>${esc(placeName(key))}</b><small>${esc(roomOf(key)||'No room yet')}</small></div><button class="pill" data-room-pick="${esc(key)}">Move</button></div>`).join('')}</div>`;
    const re=()=>{if(SHEET.type==='arrange')openSheet('arrange',null,{replace:true,focus:'keep'})};
    const ask=async fn=>{try{await fn();await loadRooms();rebuildRoomMap();re();render()}catch(e){toast(e.message,'fail')}};
    return {title:'Arrange',html,mount(root){
      $$('[data-mv]',root).forEach(b=>b.onclick=()=>{const [i,d]=b.dataset.mv.split(':').map(Number);const o=ROOM_ORDER.slice();const j=i+d;if(j<0||j>=o.length)return;[o[i],o[j]]=[o[j],o[i]];const a=ACT.reorder(o);a.onDone=re;dispatch(a,{channel:'touch'})});
      $('#ar-add',root).onclick=()=>{const name=$('#ar-new',root).value.trim();if(!name)return;ask(()=>api('POST','/rooms',{name}))};
      $$('[data-ren]',root).forEach(b=>b.onclick=()=>{const r=roomByName(ROOM_ORDER[+b.dataset.ren]);const name=$('#ar-new',root).value.trim();if(!name){toast('Type the new name in the field below, then tap the pencil.','');$('#ar-new',root).focus();return}ask(()=>api('PATCH','/rooms/'+enc(r.id),{name}))});
      $$('[data-del]',root).forEach(b=>b.onclick=()=>{const r=roomByName(ROOM_ORDER[+b.dataset.del]);if(b.dataset.sure!=='1'){b.dataset.sure='1';toast('Tap again to delete the '+r.name+'. Its lights stay, with no room.','');setTimeout(()=>{delete b.dataset.sure},4000);return}ask(()=>api('DELETE','/rooms/'+enc(r.id)))})}};
  },
  room(name){
    const r=name;const bs=roomBulbs(r);const RL=lightsSummary(bs);const pinned=pinnedRoom()===r;const defs=roomSceneDefs(r);const savedAlready=(roomByName(r)||{scenes:[]}).scenes.some(s=>s.name==='Saved');
    const html=`${failHtml('room-'+slug(r))}
      ${bs.length?`<div class="segc" role="group" aria-label="${esc(r)} scenes">${Object.keys(defs).map(n=>`<button data-rscene="${n}" aria-pressed="${scenePressed(r)===n}">${n}</button>`).join('')}</div>`:`<p class="quiet">No lights in the ${esc(r)} yet.${isOwner()?' Add one under Find new lights, or move one here under Arrange.':''}</p>`}
      <div>${bs.map(b=>`<div class="bulbrow ${b.on?'':'off'}" data-bulb="${b.id}">${failHtml('bulb'+b.id)}
        <div class="bh"><span class="nm"><i style="--h:${b.hue}"></i><span data-bname>${esc(bulbName(b))}</span><small class="fine">${esc(b.kind)}</small><span class="pendslot">${PENDING['bulb'+b.id]?PEND:''}</span></span><span style="display:flex;gap:6px;align-items:center">${isOwner()?`<button class="pill" data-rename="${b.id}" aria-label="Rename ${esc(bulbName(b))}">${ICON('pencil')}</button><button class="pill" data-room-pick="bulb${b.id}" aria-label="Move ${esc(bulbName(b))} to another room">Move</button>`:''}<button class="toggle" role="switch" aria-checked="${b.on}" aria-label="${esc(bulbName(b))}" data-tog="${b.id}"></button></span></div>
        <div class="inline-edit" data-edit="${b.id}" hidden><input class="field" id="rn-${b.id}" aria-label="New name" maxlength="24" value="${esc(bulbName(b))}"><button class="btn primary" data-rename-ok="${b.id}">Save</button></div>
        ${sliderEl('sl-b'+b.id,b.on?b.bri:0,true,b.hue,'Brightness',!b.known?NR:b.on?b.bri+'%':'Off')}
      </div>`).join('')}</div>
      <div class="rows">
        <div class="rowi">${ICON('pin')}<div class="t"><b>Open Home on this room</b><small>${!bs.length?'Add a light first.':pinned?'Home opens here for '+esc(person().name)+', until another room is picked on it.':pinnedRoom()?'This replaces the '+esc(pinnedRoom())+'.':'Home opens on all the lights until you do.'}</small></div><button class="toggle" role="switch" aria-checked="${pinned}" aria-label="Open Home on the ${esc(r)}" id="rm-pin" ${bs.length?'':'disabled'}></button></div>
        ${isOwner()&&bs.length?row('heart','Save these levels as this room’s scene',savedAlready?'Replaces the saved scene':'Adds a Saved scene above',null,'data-savescene="1"'):''}
      </div>`;
    return {title:r,html,glowHue:RL.hue,glowA:RL.n?0.26:0,
      mount(root){
        $$('[data-rscene]',root).forEach(b=>b.onclick=()=>runRoomScene(r,b.dataset.rscene,{channel:'touch'}));
        $$('[data-tog]',root).forEach(t=>t.onclick=()=>{const n=+t.dataset.tog;dispatch(ACT.bulbOn(n,!bulbById(n).on),{channel:'touch'})});
        bs.forEach(b=>wireSlider($('#sl-b'+b.id,root),{get:()=>b.on?b.bri:0,fmt:v=>v?v+'%':'Off',set:v=>dispatch(ACT.bulbBri(b.id,v),{channel:'touch'})}));
        $$('[data-rename]',root).forEach(b=>b.onclick=()=>{const ed=$(`[data-edit="${b.dataset.rename}"]`,root);ed.hidden=!ed.hidden;if(!ed.hidden)$('input',ed).focus()});
        const save=id=>{const inp=$('#rn-'+id,root);const nm=inp.value.trim().slice(0,24);if(!nm)return;const a=ACT.rename(id,nm);a.onDone=()=>{if(SHEET.type==='room:'+r)openSheet('room:'+r,null,{replace:true,focus:'keep'})};dispatch(a,{channel:'touch'});$(`[data-edit="${id}"]`,root).hidden=true};
        $$('[data-rename-ok]',root).forEach(b=>b.onclick=()=>save(+b.dataset.renameOk));
        $$('[data-edit] input',root).forEach(i=>i.addEventListener('keydown',e=>{if(e.key==='Enter'){e.preventDefault();save(+i.closest('[data-edit]').dataset.edit)}}));
        $('#rm-pin',root).onclick=()=>{if(!bs.length)return;const on=$('#rm-pin',root).getAttribute('aria-checked')!=='true';const a=ACT.pin(r,on);const prev=a.onDone;a.onDone=()=>{prev();if(SHEET.type==='room:'+r)openSheet('room:'+r,null,{replace:true,focus:'keep'})};dispatch(a,{channel:'touch'})};
        const ss=$('[data-savescene]',root);if(ss)ss.onclick=()=>{const a=ACT.saveRoomScene(r);a.onDone=()=>{if(SHEET.type==='room:'+r)openSheet('room:'+r,null,{replace:true,focus:'keep'})};dispatch(a,{channel:'touch'})}},
      update(){const root=$('#sbody');$$('[data-rscene]',root).forEach(b=>b.setAttribute('aria-pressed',String(scenePressed(r)===b.dataset.rscene)));
        bs.forEach(b=>{const el=$(`[data-bulb="${b.id}"]`,root);if(!el)return;el.classList.toggle('off',!b.on);$('.nm i',el).style.setProperty('--h',b.hue);$('.pendslot',el).innerHTML=PENDING['bulb'+b.id]?PEND:'';const t=$('[data-tog]',el);t.setAttribute('aria-checked',String(b.on));t.classList.toggle('pending',!!PENDING['bulb'+b.id]);
          const sl=$('#sl-b'+b.id,el);if(sl&&!sl.classList.contains('dragging')){const p=lean('bulb'+b.id,'bri',b.on?b.bri:0);const on=lean('bulb'+b.id,'on',b.on?1:0);const v=on>0.5?p:0;sl.style.setProperty('--p',v.toFixed(1));sl.style.setProperty('--gh',b.hue);sl.setAttribute('aria-valuenow',Math.round(v));$$('.num',sl).forEach(n=>n.textContent=!b.known?NR:v?Math.round(v)+'%':'Off')}});
        const pin=$('#rm-pin',root);if(pin){const on=pinnedRoom()===r;pin.setAttribute('aria-checked',String(on));pin.classList.toggle('pending',!!PENDING['pin-'+slug(r)])}
        const ok=lightsSummary(bs);SHEET.el.style.setProperty('--gh',ok.hue);SHEET.el.style.setProperty('--ga',ok.n?0.26:0)}};
  },
  where(key){
    const t=targetOfKey(key);const b=/^bulb\d+$/.test(key)?bulbById(key.replace('bulb','')):null;const name=b?bulbName(b):placeName(key);const cur=roomOf(key);
    const html=`<div class="rows">${ROOM_ORDER.map(r=>row(r===cur?'check':null,esc(r),null,null,`data-pick="${esc(r)}"`)).join('')}${row(cur?null:'check','No room','Not in any room',null,'data-pick=""')}</div>${isOwner()?'<div class="inline-edit"><input class="field" id="wh-new" placeholder="A new room" aria-label="New room name" maxlength="40" autocomplete="off"><button class="btn primary" id="wh-add">Add</button></div>':''}`;
    /* the previous page comes back only once the move has applied, so it never shows the old room */
    return {title:'Where is '+name+'?',html,mount(root){$$('[data-pick]',root).forEach(x=>x.onclick=()=>{const a=ACT.room(key,x.dataset.pick||null);a.onDone=()=>sheetBack();if(dispatch(a,{channel:'touch'})){x.classList.add('sel');x.insertAdjacentHTML('beforeend',PEND)}});
      const add=$('#wh-add',root);if(add)add.onclick=async()=>{const nm=$('#wh-new',root).value.trim();if(!nm)return;add.disabled=true;try{await api('POST','/rooms',{name:nm});await loadRooms();const a=ACT.room(key,nm);a.onDone=()=>sheetBack();dispatch(a,{channel:'touch'})}catch(e){add.disabled=false;toast(e.message,'fail')}}}};
  },
};

export {placeName,placeRow,roomsSheets};

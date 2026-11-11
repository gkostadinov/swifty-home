import {$,esc} from '../core/util.js';
import {NR,D} from '../core/store.js';
import {PENDING} from '../core/actions.js';
import {ACT} from '../core/catalog.js';
import {dispatch} from '../core/guards.js';
import {SHEET,openSheet,row,roomOf} from '../ui/sheets.js';
import {notesBlock,wireNotes} from './index.js';

const plugSheets={
  plug(target){
    const p=D.plugs.find(x=>x.target===target);if(!p)return {title:'Socket',html:'<p class="quiet">This socket is not reporting.</p>'};
    const html=`<div class="rows"><div class="rowi"><div class="t"><b>Power</b><small id="pl-word"></small></div><button class="toggle" role="switch" id="pl-tog" aria-label="${esc(p.name)}"></button></div></div>
      <h3 class="roomhead" style="font-size:17px">Name</h3>
      <div class="inline-edit"><input class="field" id="pl-name" maxlength="24" value="${esc(p.name)}" aria-label="Name" autocomplete="off"><button class="btn primary" id="pl-save">Save</button></div>
      <h3 class="roomhead" style="font-size:17px">Room</h3>
      <div class="rows">${row('rooms',esc(roomOf(target)||'No room'),'Tap to change',null,`data-open="where:${esc(target)}"`)}</div>
      ${notesBlock(target)}`;
    return {title:p.name,html,
      mount(root){$('#pl-tog',root).onclick=()=>{const x=D.plugs.find(y=>y.target===target);if(x&&x.known)dispatch(ACT.plug(target,!x.on),{channel:'touch'})};
        const save=()=>{const nm=$('#pl-name',root).value.trim().slice(0,24);if(!nm||nm===p.name)return;const a=ACT.renameDevice(target,nm);a.onDone=()=>{if(SHEET.type==='plug:'+target)openSheet('plug:'+target,null,{replace:true,focus:'keep'})};dispatch(a,{channel:'touch'})};
        $('#pl-save',root).onclick=save;wireNotes(root);$('#pl-name',root).addEventListener('keydown',e=>{if(e.key==='Enter'){e.preventDefault();save()}});
        this.update()},
      update(){const root=$('#sbody');const x=D.plugs.find(y=>y.target===target);const t=$('#pl-tog',root);if(!x||!t)return;t.setAttribute('aria-checked',String(!!x.on));t.disabled=!x.known;t.classList.toggle('pending',!!PENDING[target]);$('#pl-word',root).textContent=x.known?(x.on?'On':'Off'):(x.reason||NR)}};
  },
};

export {plugSheets};

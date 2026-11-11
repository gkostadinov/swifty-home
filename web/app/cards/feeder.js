import {$,esc,ICON,PEND} from '../core/util.js';
import {NR,D,usable,pluginLabel} from '../core/store.js';
import {PENDING} from '../core/actions.js';
import {ACT,portions} from '../core/catalog.js';
import {armGuard} from '../core/guards.js';
import {failHtml} from '../ui/sheets.js';
import {placeRow} from '../pages/rooms.js';
import {litterHtml,wireLitter,updateLitter} from './litter.js';
import {notesBlock,wireNotes} from './index.js';

/* ============================================================ pets: the feeder, with the litter box beside it. A feed is the smallest portion the feeder takes */
const feederSheets={
  pets(arg,opts){
    const feedWord=()=>D.feeder.known?(D.feeder.last?`Last feed ${D.feeder.last} · ${D.feeder.portions} portions today`:`${D.feeder.portions} portions today`):(D.feeder.reason||NR);
    const feedOff=()=>!!PENDING.feeder||!usable(D.feeder.target);const g=portions()[0];
    const facts=[D.feeder.target&&pluginLabel(D.feeder.target)?`<dt>${esc(D.feeder.name)}</dt><dd>${esc(pluginLabel(D.feeder.target))}</dd>`:'',D.litter.target&&pluginLabel(D.litter.target)?`<dt>${esc(D.litter.name)}</dt><dd>${esc(pluginLabel(D.litter.target))}</dd>`:''].join('');
    const html=`${D.feeder.target?`${failHtml('feeder')}
      <div class="rowi">${ICON('paw')}<div class="t"><b>${esc(D.feeder.name)}</b><small id="feed-st">${esc(feedWord())}</small></div><span id="feed-pend">${PENDING.feeder?PEND:''}</span></div>
      <div class="acts" id="feed-acts"><button class="btn primary" id="feedbtn" ${feedOff()?'disabled':''}>Feed ${g} g</button></div>`:''}
      ${litterHtml()}
      ${D.feeder.target?placeRow(D.feeder.target,D.feeder.name):''}${D.litter.target?placeRow(D.litter.target,D.litter.name):''}
      ${notesBlock(D.feeder.target,D.litter.target?D.feeder.name.toLowerCase():'')}${notesBlock(D.litter.target,D.feeder.target?D.litter.name.toLowerCase():'')}
      ${facts?`<details class="disc"><summary>Details<svg class="ic chev" aria-hidden="true"><use href="#i-chev"/></svg></summary><dl class="facts">${facts}</dl></details>`:''}
      ${!D.feeder.target&&!D.litter.target?'<p class="quiet">There is no feeder or litter box here yet.</p>':''}`;
    return {title:'Pets',html,
      mount(root){const fb=$('#feedbtn',root);if(fb)fb.onclick=()=>armGuard(ACT.feed(g),$('#feed-acts',root),{channel:'touch'});wireLitter(root);wireNotes(root);
        if(opts.action){const at=opts.action.key==='feeder'?$('#feed-acts',root):$('#litter-acts',root);if(at)armGuard(opts.action,at,{channel:opts.channel,transcript:opts.transcript})}},
      update(){const root=$('#sbody');const f=$('#feed-st',root);if(f)f.textContent=feedWord();const fp=$('#feed-pend',root);if(fp)fp.innerHTML=PENDING.feeder?PEND:'';const fb=$('#feedbtn',root);if(fb)fb.disabled=feedOff();updateLitter(root)}};
  },
};

export {feederSheets};

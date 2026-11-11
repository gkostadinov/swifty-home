import {$,esc,ICON,PEND} from '../core/util.js';
import {NR,D,usable} from '../core/store.js';
import {PENDING} from '../core/actions.js';
import {ACT} from '../core/catalog.js';
import {armGuard} from '../core/guards.js';
import {failHtml} from '../ui/sheets.js';

/* ============================================================ the litter box: its part of the Pets page */
const litterWord=()=>D.litter.known?(D.litter.busy?'Cleaning now':D.litter.last?'Last clean cycle '+D.litter.last:'No cycle yet'):(D.litter.reason||NR);
const cleanOff=()=>!!PENDING.litter||D.litter.busy||!usable(D.litter.target);
function litterHtml(){if(!D.litter.target)return '';return `${failHtml('litter')}
      <div class="rowi">${ICON('paw')}<div class="t"><b>${esc(D.litter.name)}</b><small id="litter-st">${esc(litterWord())}</small></div><span id="litter-pend">${PENDING.litter?PEND:''}</span></div>
      <div class="acts" id="litter-acts"><button class="btn" id="cleanbtn" ${cleanOff()?'disabled':''}>Clean cycle</button></div>`}
function wireLitter(root){const b=$('#cleanbtn',root);if(b)b.onclick=()=>armGuard(ACT.clean(),$('#litter-acts',root),{channel:'touch'})}
function updateLitter(root){const l=$('#litter-st',root);if(!l)return;l.textContent=litterWord();$('#litter-pend',root).innerHTML=PENDING.litter?PEND:'';const cb=$('#cleanbtn',root);if(cb)cb.disabled=cleanOff()}

export {litterWord,litterHtml,wireLitter,updateLitter};

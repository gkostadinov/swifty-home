import {$,esc,ICON,PEND} from '../core/util.js';
import {S,NR,T,TGT,pluginLabel} from '../core/store.js';
import {PENDING} from '../core/actions.js';
import {ACT} from '../core/catalog.js';
import {dispatch,armGuard} from '../core/guards.js';
import {failHtml} from '../ui/sheets.js';
import {placeRow} from '../pages/rooms.js';
import {notesBlock,wireNotes} from './index.js';

const printerSheets={
  printer(arg,opts){
    const p=T.printer;const f=(v,u)=>v==null||!T.printer.known?NR:Math.round(v)+u;
    const html=`${failHtml('printer')}<div id="pr-body"></div><p class="p2">The printer confirms each change before it shows here.</p>${notesBlock(TGT.printer)}${placeRow(TGT.printer)}<details class="disc"><summary>Details<svg class="ic chev" aria-hidden="true"><use href="#i-chev"/></svg></summary><dl class="facts"><dt>Name</dt><dd>${esc(T.printer.name)}${pluginLabel(TGT.printer)?' · '+esc(pluginLabel(TGT.printer)):''}</dd><dt>Nozzle</dt><dd><span id="pr-noz" class="num"></span></dd><dt>Bed</dt><dd><span id="pr-bed" class="num"></span></dd></dl></details>`;
    const bodyKey=()=>T.printer.known+'|'+T.printer.state+'|'+(PENDING.printer?1:0)+'|'+Math.round(T.printer.pct||0)+'|'+S.live;
    const word={printing:'Printing',paused:'Paused',idle:'Idle',stopped:'Stopped',error:'Error',starting:'Starting',unknown:'Unknown'};
    const body=()=>{const p=T.printer;
      if(!p.known){const lost=S.live&&p.was;return `<p class="sent">${lost?`Stopped responding ${p.lostAt?Math.max(1,Math.round((Date.now()-p.lostAt)/60e3))+' min ago':'just now'}.`:NR+'.'}</p><p class="p2">${esc(p.reason||'')}${lost&&p.layers?` It last reported layer ${p.layer} of ${p.layers}.`:''}</p><div class="acts" id="pr-acts"><button class="btn primary" data-pr="reconnect" ${PENDING.printer?'disabled':''}>${PENDING.printer?PEND+'<span>Checking</span>':'Check again'}</button>${S.live?'':'<button class="btn" data-pr="start">Start the loaded job</button><button class="btn danger" data-pr="estop" style="flex-basis:100%">Emergency stop</button>'}</div>`}
      const active=p.state==='printing'||p.state==='paused';
      return `<div class="rowi">${ICON('printer')}<div class="t"><b>${esc(word[p.state]||p.state)}${active&&p.layers?' · layer '+p.layer+' of '+p.layers:''}</b><small>${p.state==='printing'?Math.round(p.pct)+'% done':p.state==='paused'?'Heaters still on':'Waiting'}</small></div><span>${PENDING.printer?PEND:''}</span></div>
        <div class="slider" style="height:14px;border-radius:7px;pointer-events:none" aria-hidden="true"><div class="sfill" style="--p:${(p.pct||0).toFixed(1)}"></div></div>
        <div class="acts" id="pr-acts">${p.state==='printing'?'<button class="btn" data-pr="pause">Pause</button>':p.state==='paused'?'<button class="btn primary" data-pr="pause">Resume</button>':'<button class="btn primary" data-pr="start">Start the loaded job</button>'}${active?'<button class="btn danger" data-pr="estop" style="flex-basis:100%">Emergency stop</button>':''}</div>`};
    const paintFacts=root=>{const n=$('#pr-noz',root);if(n){n.textContent=f(T.printer.nozzle,' °C');$('#pr-bed',root).textContent=f(T.printer.bed,' °C')}};
    const lost=S.live&&p.was&&!p.known;
    return {title:'Printer',html,glowHue:lost?38:270,glowA:lost?0.2:0.08,
      mount(root){const b=$('#pr-body',root);b.innerHTML=body();b.dataset.key=bodyKey();paintFacts(root);wireNotes(root);
        /* one delegated handler on the body, so a re-render can never strip it */
        b.addEventListener('click',e=>{const x=e.target.closest('[data-pr]');if(!x||x.disabled)return;const k=x.dataset.pr;const acts=$('#pr-acts',b);if(k==='reconnect')dispatch(ACT.reconnect(),{channel:'touch'});else if(k==='pause')armGuard(ACT.printPause(),acts,{channel:'touch'});else if(k==='start')armGuard(ACT.printStart(),acts,{channel:'touch'});else armGuard(ACT.estop(),acts,{channel:'touch'})});
        if(opts.action)armGuard(opts.action,$('#pr-acts',b),{channel:opts.channel,transcript:opts.transcript})},
      update(){const root=$('#sbody');const b=$('#pr-body',root);if(!b)return;paintFacts(root);if($('[data-guard]',b))return;const key=bodyKey();if(b.dataset.key!==key){b.dataset.key=key;b.innerHTML=body()}}};
  },
};

export {printerSheets};

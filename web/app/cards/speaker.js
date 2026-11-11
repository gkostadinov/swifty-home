import {$,$$,esc,ICON} from '../core/util.js';
import {NR,D,hasCap,pluginLabel} from '../core/store.js';
import {lean} from '../ui/roomlight.js';
import {PENDING} from '../core/actions.js';
import {ACT} from '../core/catalog.js';
import {sliderEl,wireSlider} from '../ui/gestures.js';
import {dispatch} from '../core/guards.js';
import {roomOf,failHtml} from '../ui/sheets.js';
import {placeRow} from '../pages/rooms.js';
import {tvInputs,tvHtml,wireTv,updateTv} from './tv.js';
import {notesBlock,wireNotes} from './index.js';

/* ============================================================ the speaker, with the TV beside it: one page for what plays in the room */
/* its controls are the ones it says it has: volume, mute and night mode */
const speakerSheets={
  music(arg,opts){opts=opts||{};
    const sp=D.speaker.target;const has=c=>!!sp&&hasCap(sp,c);
    const muteWord=()=>D.speaker.known?(D.speaker.mute?'Muted':'Playing at '+D.speaker.vol):(D.speaker.reason||NR);
    const nightWord=()=>D.speaker.night?'On. Loud sounds are softer.':'Off';
    const speakerHtml=!sp?'':`${failHtml('speaker')}
      <h3 class="roomhead">${esc(D.speaker.name)}</h3>
      ${has('volume')?sliderEl('sl-vol',D.speaker.vol,false,0,'Volume',D.speaker.known?String(D.speaker.vol):NR):''}
      <div class="rows">
        ${has('mute')?`<div class="rowi">${ICON('speaker')}<div class="t"><b>Mute</b><small id="mute-st">${esc(muteWord())}</small></div><button class="toggle" role="switch" aria-checked="${D.speaker.mute}" aria-label="Mute" id="mute-tog"></button></div>`:`<div class="rowi">${ICON('speaker')}<div class="t"><b>${esc(D.speaker.known?'Answering':(D.speaker.reason||NR))}</b></div></div>`}
        ${has('night_mode')?`<div class="rowi" id="night-row" ${D.speaker.night==null?'hidden':''}>${ICON('speaker')}<div class="t"><b>Night mode</b><small id="night-st">${esc(nightWord())}</small></div><button class="toggle" role="switch" aria-checked="${!!D.speaker.night}" aria-label="Night mode" id="night-tog"></button></div>`:''}
      </div>`;
    const facts=[sp?`<dt>Speaker</dt><dd>${esc(D.speaker.name)}${roomOf(sp)?', in the '+esc(roomOf(sp)):''}${pluginLabel(sp)?' · '+esc(pluginLabel(sp)):''}</dd>`:'',D.tv.target?`<dt>TV</dt><dd>${esc(D.tv.name)}${pluginLabel(D.tv.target)?' · '+esc(pluginLabel(D.tv.target)):''}</dd>`:'',D.tv.target&&hasCap(D.tv.target,'inputs')?`<dt>Inputs</dt><dd>${esc(tvInputs().join(', '))}</dd>`:''].join('');
    const html=`${speakerHtml}
      ${tvHtml()}
      ${sp?placeRow(sp,D.speaker.name):''}${D.tv.target?placeRow(D.tv.target,D.tv.name):''}
      ${notesBlock(sp,D.tv.target?D.speaker.name.toLowerCase():'')}${notesBlock(D.tv.target,sp?D.tv.name:'')}
      ${facts?`<details class="disc"><summary>Details<svg class="ic chev" aria-hidden="true"><use href="#i-chev"/></svg></summary><dl class="facts">${facts}</dl></details>`:''}
      ${!sp&&!D.tv.target?'<p class="quiet">There is no speaker or TV here yet.</p>':''}`;
    return {title:sp&&D.tv.target?'Music & TV':D.tv.target?D.tv.name:'Music',html,glowHue:270,glowA:0.12,
      mount(root){
        const sl=$('#sl-vol',root);if(sl)wireSlider(sl,{get:()=>D.speaker.vol,fmt:v=>String(v),set:v=>dispatch(ACT.vol(v),{channel:'touch'})});
        const mt=$('#mute-tog',root);if(mt)mt.onclick=()=>dispatch(ACT.mute(!D.speaker.mute),{channel:'touch'});
        const nt=$('#night-tog',root);if(nt)nt.onclick=()=>dispatch(ACT.night(!D.speaker.night),{channel:'touch'});
        wireTv(root,opts);wireNotes(root)},
      update(){const root=$('#sbody');const sl=$('#sl-vol',root);if(sl&&!sl.classList.contains('dragging')){const v=lean('speaker','vol',D.speaker.vol);sl.style.setProperty('--p',v.toFixed(1));sl.setAttribute('aria-valuenow',Math.round(v));$$('.num',sl).forEach(n=>n.textContent=D.speaker.known?String(Math.round(v)):NR)}
        const mt=$('#mute-tog',root);if(mt){mt.setAttribute('aria-checked',String(D.speaker.mute));mt.classList.toggle('pending',!!PENDING.speaker);$('#mute-st',root).textContent=muteWord()}
        const nr=$('#night-row',root);if(nr){nr.hidden=D.speaker.night==null;const nt=$('#night-tog',root);nt.setAttribute('aria-checked',String(!!D.speaker.night));nt.classList.toggle('pending',!!PENDING.speaker);$('#night-st',root).textContent=nightWord()}
        updateTv(root)}};
  },
};

export {speakerSheets};

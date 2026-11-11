import {$,$$,clamp,esc,ICON} from '../core/util.js';
import {NR,D,pluginOf} from '../core/store.js';
import {acRange,acModes,acFans,acWord} from '../pages/home.js';
import {PENDING} from '../core/actions.js';
import {ACT} from '../core/catalog.js';
import {dispatch} from '../core/guards.js';
import {SHEET,openSheet} from '../ui/sheets.js';
import {placeRow} from '../pages/rooms.js';
import {notesBlock,wireNotes} from './index.js';

const airconSheets={
  ac(target){
    const c0=D.acs.find(x=>x.target===target);if(!c0)return {title:'Air conditioner',html:'<p class="quiet">This air conditioner is not reporting.</p>'};
    const head=t=>`<h3 class="roomhead" style="font-size:17px">${t}</h3>`;
    /* only the modes, fan speeds and range this unit says it has; a cloud integration says whose cloud it goes through */
    const [AC_LOW,AC_HIGH]=acRange(target);const p=pluginOf(target);const reach=p&&p.reach;const cloud=reach&&reach.reach==='cloud';
    const html=`<div class="rows"><div class="rowi">${ICON('snow')}<div class="t"><b>Power</b><small id="ac-word"></small></div><button class="toggle" role="switch" id="ac-tog" aria-label="${esc(c0.name)}"></button></div></div>
      ${head('Mode')}<div class="segc" role="group" aria-label="Mode" id="ac-modes">${acModes(target).map(([m,l])=>`<button data-acmode="${m}" aria-pressed="false">${l}</button>`).join('')}</div>
      ${head('Temperature')}<div class="rows"><div class="rowi"><div class="t"><b class="num" id="ac-temp"></b><small id="ac-temps"></small></div><span style="display:flex;gap:8px"><button class="pill" id="ac-down" aria-label="Cooler">−</button><button class="pill" id="ac-up" aria-label="Warmer">+</button></span></div></div>
      ${head('Fan')}<div class="segc fans" role="group" aria-label="Fan speed" id="ac-fans">${acFans(target).map(([f,l])=>`<button data-acfan="${f}" aria-pressed="false">${l}</button>`).join('')}</div>
      ${head('Name')}<div class="inline-edit"><input class="field" id="ac-name" maxlength="24" value="${esc(c0.name)}" aria-label="Name" autocomplete="off"><button class="btn primary" id="ac-save">Save</button></div>
      ${head('Room')}${placeRow(target,'Tap to change')}
      ${notesBlock(target)}
      <p class="p2">${cloud&&reach.vendor?'Changes go through '+esc(reach.vendor)+'’s cloud, so they can take a few seconds to show.':'Changes can take a few seconds to show.'}</p>`;
    /* a number being stepped: shown at once, and sent once the taps stop, so each tap is not a trip to the cloud */
    let stepping=null,stepT=0;
    const current=()=>D.acs.find(y=>y.target===target);
    return {title:c0.name,html,
      mount(root){$('#ac-tog',root).onclick=()=>{const x=current();if(x&&x.known)dispatch(ACT.acPower(target,!x.on),{channel:'touch'})};
        $$('[data-acmode]',root).forEach(b=>b.onclick=()=>dispatch(ACT.acMode(target,b.dataset.acmode),{channel:'touch'}));
        $$('[data-acfan]',root).forEach(b=>b.onclick=()=>dispatch(ACT.acFan(target,b.dataset.acfan),{channel:'touch'}));
        const step=d=>{const x=current();if(!x||!x.known)return;const from=stepping!=null?stepping:(x.temp==null?22:x.temp);stepping=clamp(from+d,AC_LOW,AC_HIGH);this.update();clearTimeout(stepT);stepT=setTimeout(()=>{const v=stepping;stepping=null;dispatch(ACT.acTemp(target,v),{channel:'touch'})},600)};
        $('#ac-down',root).onclick=()=>step(-1);$('#ac-up',root).onclick=()=>step(1);
        const save=()=>{const nm=$('#ac-name',root).value.trim().slice(0,24);if(!nm||nm===c0.name)return;const a=ACT.renameDevice(target,nm);a.onDone=()=>{if(SHEET.type==='ac:'+target)openSheet('ac:'+target,null,{replace:true,focus:'keep'})};dispatch(a,{channel:'touch'})};
        $('#ac-save',root).onclick=save;wireNotes(root);$('#ac-name',root).addEventListener('keydown',e=>{if(e.key==='Enter'){e.preventDefault();save()}});
        this.update()},
      update(){const root=$('#sbody');const x=current();const t=$('#ac-tog',root);if(!x||!t)return;const pend=!!PENDING[target];
        t.setAttribute('aria-checked',String(!!x.on));t.disabled=!x.known;t.classList.toggle('pending',pend);$('#ac-word',root).textContent=acWord(x);
        const noTarget=x.mode==='fan';const shown=stepping!=null?stepping:x.temp;$('#ac-temp',root).textContent=noTarget?'None in fan mode':(shown==null?NR:shown+'°');
        $('#ac-temps',root).textContent=[x.indoor!=null?x.indoor+'° inside':'',x.outdoor!=null?x.outdoor+'° outside':''].filter(Boolean).join(' · ');
        const dn=$('#ac-down',root),up=$('#ac-up',root);dn.disabled=!x.known||noTarget||shown==null||shown<=AC_LOW;up.disabled=!x.known||noTarget||shown==null||shown>=AC_HIGH;
        $$('[data-acmode]',root).forEach(b=>{b.setAttribute('aria-pressed',String(x.mode===b.dataset.acmode));b.disabled=!x.known});
        $$('[data-acfan]',root).forEach(b=>{b.setAttribute('aria-pressed',String(x.fan===b.dataset.acfan));b.disabled=!x.known})}};
  },
};

export {airconSheets};

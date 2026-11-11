import {$,$$,esc,ICON,PEND} from '../core/util.js';
import {CAT,isOwner,NR,D,capOf,hasCap,tvLabel} from '../core/store.js';
import {inView} from '../pages/home.js';
import {PENDING} from '../core/actions.js';
import {ACT} from '../core/catalog.js';
import {dispatch,armGuard} from '../core/guards.js';
import {failHtml} from '../ui/sheets.js';

/* ============================================================ the TV: the shortcut bar on Home and its part of the Music & TV page */
/* what this TV offers: its inputs and remote keys come from its capabilities */
const tvInputs=()=>{const c=capOf(D.tv.target,'inputs');return (c&&Array.isArray(c.inputs)&&c.inputs.length?c.inputs:['HDMI 1','HDMI 2','HDMI 3','Live TV']).map(tvLabel)};
const offersKey=k=>{const c=capOf(D.tv.target,'keys');return !c||(c.keys||[]).includes(k)};
/* the TV's shortcut on Home: only once the TV is answering. Turning it on or off still goes through its confirmation */
function paintTvBar(){const el=$('#tvbar');const show=!!D.tv.target&&D.tv.known&&inView(D.tv.target)&&(isOwner()||!!CAT['tv.key']);el.hidden=!show;if(!show)return;
  const key=(k,label,icon)=>offersKey(k)?`<button class="rk" data-tvk="${k}" aria-label="${label}">${ICON(icon)}</button>`:'';
  if(!el.firstChild)el.innerHTML=`<button class="tvname" data-tvbar="open" aria-label="Open the full TV remote"><b>${esc(D.tv.name)}</b><small></small></button>${hasCap(D.tv.target,'power')?'<button class="rk" data-tvbar="power"><svg class="ic" aria-hidden="true"><use href="#i-power"/></svg></button>':''}${key('play','Play','play')}${key('pause','Pause','pause')}${key('volume_down','Volume down','voldown')}${key('volume_up','Volume up','volup')}`;
  const on=D.tv.on;$('.tvname small',el).textContent=on?(D.tv.input||'On'):'Off';const p=$('[data-tvbar="power"]',el);if(p){p.classList.toggle('on',on);p.setAttribute('aria-label',on?'Turn off the TV':'Turn on the TV')}$$('[data-tvk]',el).forEach(b=>b.disabled=!on)}
/* the TV's part of the Music & TV page */
const tvWord=()=>D.tv.known?(D.tv.on?'On · '+esc(D.tv.input):'Off'):NR;
/* a TV that has not been contacted yet is only waiting for someone to ask: pressing a button is asking */
const remoteReady=()=>D.tv.known?D.tv.on:!!D.tv.uncontacted;
const remoteWhy=()=>D.tv.known?'The TV is off.':(D.tv.reason||NR);
function tvHtml(){if(!D.tv.target)return '';
  const remoteKey=(k,label,inner)=>offersKey(k)?`<button data-key="${k}" aria-label="${label}" ${remoteReady()?'':'disabled'}>${inner||label}</button>`:'';
  return `<h3 class="roomhead">${esc(D.tv.name)}</h3>
      ${failHtml('tv')}
      <div class="rows">
        <div class="rowi">${ICON('tv')}<div class="t"><b id="tv-word">${tvWord()}</b><small>${esc(D.tv.known?(D.speaker.target?'The speaker plays what the TV plays':'Answering'):(D.tv.reason||NR))}</small></div><span id="tv-acts" style="display:flex;gap:8px">${hasCap(D.tv.target,'power')?`<button class="pill ${D.tv.on?'':'on'}" data-tvpower="${D.tv.on?'off':'on'}">${D.tv.on?'Turn off':'Turn on'}</button>`:''}</span></div>
      </div>
      ${hasCap(D.tv.target,'inputs')?`<div class="segc" role="group" aria-label="Input" id="tv-inputs">${tvInputs().map(i=>`<button data-input="${esc(i)}" aria-pressed="${D.tv.input===i}">${esc(i)}</button>`).join('')}</div>`:''}
      ${hasCap(D.tv.target,'keys')?`<h3 class="roomhead">Remote</h3>
      <p class="quiet" id="remote-why" ${remoteReady()?'hidden':''}>${esc(remoteWhy())}</p>
      <div class="remote" role="group" aria-label="TV remote" id="remote">
        <div class="dpad">${remoteKey('up','Up',ICON('up'))}${remoteKey('left','Left',ICON('back'))}${remoteKey('ok','OK')}${remoteKey('right','Right',ICON('chev'))}${remoteKey('down','Down',ICON('down'))}</div>
        <div class="rkeys">${remoteKey('volume_up','Volume up','Vol +')}${remoteKey('channel_up','Channel up','Ch +')}${remoteKey('back','Back')}${remoteKey('volume_down','Volume down','Vol −')}${remoteKey('channel_down','Channel down','Ch −')}${remoteKey('home','Home')}${remoteKey('mute','Mute')}${remoteKey('play','Play')}${remoteKey('pause','Pause')}</div>
      </div>`:''}`}
function wireTv(root,opts){if(!D.tv.target)return;
  $$('#remote [data-key]',root).forEach(b=>b.onclick=()=>dispatch(ACT.tvKey(b.dataset.key,b.getAttribute('aria-label')),{channel:'touch'}));
  $$('[data-input]',root).forEach(b=>b.onclick=()=>dispatch(ACT.tvInput(b.dataset.input),{channel:'touch'}));
  const b=$('[data-tvpower]',root);if(b)b.onclick=()=>armGuard(ACT.tvPower(b.dataset.tvpower==='on'),$('#tv-acts',root).closest('.rows'),{channel:'touch'});
  if(opts.action&&opts.action.key==='tv'&&$('#tv-acts',root)){armGuard(opts.action,$('#tv-acts',root).closest('.rows'),{channel:opts.channel,transcript:opts.transcript})}}
function updateTv(root){
  const rm=$('#remote',root);if(rm){const ok=remoteReady();$$('button',rm).forEach(b=>b.disabled=!ok);const why=$('#remote-why',root);why.hidden=ok;why.textContent=remoteWhy()}
  const w=$('#tv-word',root);if(w)w.innerHTML=tvWord()+(PENDING.tv?' '+PEND:'');
  const pb=$('[data-tvpower]',root);if(pb&&!$('[data-guard]',root)){pb.dataset.tvpower=D.tv.on?'off':'on';pb.textContent=D.tv.on?'Turn off':'Turn on';pb.classList.toggle('on',!D.tv.on)}
  $$('[data-input]',root).forEach(b=>b.setAttribute('aria-pressed',String(D.tv.input===b.dataset.input)))}

export {tvInputs,offersKey,paintTvBar,tvHtml,wireTv,updateTv};

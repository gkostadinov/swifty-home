import {$,$$,esc,reduced,ICON,PEND} from './util.js';
import {ApiErr,api,passkeyGet,passkeyWords} from './api.js';
import {CAT,DEVINFO,NOTES,isOwner,loadNotes} from './store.js';
import {LASTDONE,clientChannel,run,refused,dismissFail} from './actions.js';
import {toast} from '../ui/toast.js';
import {A} from './catalog.js';
import {SHEET,springEasing,openSheet,refreshSheet} from '../ui/sheets.js';
import {whereFor} from '../swifty/voice.js';
import {chatUp,toolLine,paintChat} from '../swifty/chat.js';

/* ============================================================ dispatch through the guards */
function dispatch(a,ctx){
  ctx=ctx||{};if(!a)return;a.channel=ctx.channel||'touch';
  if(a.owner&&!isOwner()){toast('Only the owner can do that.','');return false}
  if(a.disabled){toast(a.label+' is not possible right now.'+(a.stillText?' It is '+a.stillText+'.':''),'');return false}
  if(a.refused){toast(a.refused,'fail');return false}
  if(a.level==='plain'){run(a);return true}
  return false;
}
/* a typed or spoken command runs at once. The confirmation the server asks for is given here, so only a passkey
   still needs a page. tell(state,text,key) hears how it goes: running, done, failed, or info */
function doNow(a,ctx){ctx=ctx||{};a.channel=ctx.channel||'typed';const key=(a.key||'')+':'+(a.action||a.label);
  const tell=ctx.tell||((st,t,k,sim)=>{if(st==='done'&&!sim&&!a.reachedWord)toast(t,'ok');else if(st==='failed')toast(t,'fail');else if(st==='info')toast(t,'')});
  const no=t=>{tell('failed',t,key);return Promise.resolve(false)};
  if(a.owner&&!isOwner())return no('Only the owner can do that.');
  if(a.disabled)return no(a.label+' is not possible right now.'+(a.stillText?' It is '+a.stillText+'.':''));
  if(a.refused)return no(a.refused);
  if(a.unusable)return no(a.unusable);
  const done=a.onDone,fail=a.onFail;
  a.onDone=(rt,v)=>{done&&done(rt,v);const sim=!!v&&v.phase==='simulated';tell('done',sim?a.label+': dry-run, nothing was sent.':a.label,key,sim)};
  a.onFail=t=>{fail&&fail(t);tell('failed',t,key)};
  tell('running',a.label,key);
  return (async()=>{
    if(!a.prepared&&(a.level==='confirm'||a.level==='critical')){try{await prepareAction(a,a.channel)}catch(e){refused(a,e);return false}}
    if(a.prepared&&a.prepared.needs==='passkey'){tell('info',a.label+' needs your passkey.',key);SHEET.stack=[];openSheet(whereFor(a.action,a.target),null,{replace:SHEET.open,action:a,channel:a.channel});return false}
    await run(a);return true})()}
/* what Swifty or the voice set up without running it */
function preparedAction(x){if(x.kind==='prepared'){const p=x.prepared;const a=A({action:p.action,target:p.target,params:p.params,cls:'voice',key:p.target,label:p.label,goLabel:p.label,holdLabel:'Hold to confirm',targetName:p.target_name,deviceName:p.target_name});a.prepared=p;a.level=p.needs==='passkey'?'critical':'confirm';return a}
  return A({action:x.action,target:x.target,params:x.params||{},cls:'voice',key:x.target,label:x.label||x.action,holdLabel:'Hold to confirm',targetName:x.target,deviceName:x.target})}
/* the last command typed into search, and how it went, stays under the field while the next one is typed */
const QDONE=new Map();
function qTell(st,t,key){QDONE.at=Date.now();QDONE.set(key||t,{state:st,label:t});while(QDONE.size>4)QDONE.delete(QDONE.keys().next().value);paintQDone()}
function paintQDone(){const el=$('#q-done');if(!el)return;el.hidden=!QDONE.size;el.innerHTML=[...QDONE.values()].map(toolLine).join('')}
/* the same inside Swifty's answer: a line that turns from a spinner into a tick or a cross */
const chatTell=msg=>(st,t,key)=>{msg.tools=msg.tools||[];let x=msg.tools.find(y=>y.id===key);if(!x){x={id:key};msg.tools.push(x)}x.state=st;x.label=t;if(st==='failed')msg.mood='sorry';else if(st==='done')msg.mood=msg.mood||'pleased';if(chatUp())paintChat()};
/* the words the server wants shown before anything happens */
/* who or what the action reaches, unless the words above already name them (removing an integration lists its devices first) */
const affectedLine=(p,said)=>{const left=(p.affected||[]).filter(n=>!said.some(l=>String(l).includes(n)));return left.length?'Affected: '+left.join(', ')+'.':''};
function preparedWords(p){const list=[p.what].concat(p.consequences||[]);const aff=affectedLine(p,list);if(aff)list.push(aff);if(p.warning)list.push(p.warning);return list.filter(Boolean).join(' ')}
async function prepareAction(a,channel){
  const body={action:a.action,target:a.target,params:a.params||{},channel:clientChannel(channel)};let p;
  try{p=await api('POST','/actions/prepare',body)}
  catch(e){if(e.code!=='acknowledgement_required')throw e;a.params=Object.assign({},a.params,{[(e.detail&&e.detail.field)||'acknowledge_console_offline']:true});body.params=a.params;p=await api('POST','/actions/prepare',body)}
  a.prepared=p;a.consequence=preparedWords(p);a.level=p.needs==='passkey'?'critical':'confirm';return p;
}
/* the same words on a confirm page: what happens, then each consequence on its own line. The owner's notes on the device are
   among the server's consequences; they are marked as notes, so it is clear they are the owner's own words */
function guardWords(a){const p=a.prepared;const d=CAT[a.action];let lead,lines,warn='';
  if(p){lead=p.what;lines=(p.consequences||[]).slice();const aff=affectedLine(p,[p.what].concat(lines));if(aff)lines.push(aff);warn=p.warning||''}
  else{lead=d?d.description:a.label;lines=((d&&d.consequences)||[]).slice();for(const l of NOTES[a.target]||[])if(!lines.includes(l))lines.push(l)}
  const notes=new Set(NOTES[a.target]||[]);
  return `<p class="sent">${esc(lead||a.label)}</p>${lines.length?`<ul class="conseq">${lines.map(l=>notes.has(l)?`<li class="note">${ICON('pencil')}<span>${esc(l)}<small>Your note</small></span></li>`:`<li><span>${esc(l)}</span></li>`).join('')}</ul>`:''}${warn?`<p class="warnline">${ICON('warn')}<span>${esc(warn)}</span></p>`:''}`}
/* a passkey touch bound to this exact prepared action: fresh every time, never remembered */
async function stepUp(a){const p=a.prepared;const so=await api('POST','/auth/step-up/start',{prepared_id:p.prepared_id,action:p.action,target:p.target,params:p.params});a.assertion=await passkeyGet(so.options)}
function pkStep(host,label,a,onOk,channel){
  host.innerHTML=`<div class="pk"><button class="g circ press" data-pk aria-label="Touch to use your passkey"><span class="needle" aria-hidden="true"></span><svg class="ic dev" aria-hidden="true"><use href="#i-key"/></svg><svg class="ic tick" aria-hidden="true" style="width:40px;height:40px"><use href="#i-check"/></svg></button><span class="lbl" data-pklbl>${esc(label||'Touch to use your passkey')}</span></div><button class="btn quiet block" data-pkcancel>Cancel</button>`;
  const pk=$('[data-pk]',host),lbl=$('[data-pklbl]',host);
  pk.onclick=async()=>{if(pk.classList.contains('sweep'))return;pk.classList.add('sweep');lbl.textContent='Checking…';
    try{if(!a.prepared)await prepareAction(a,channel||'touch');await stepUp(a);pk.classList.remove('sweep');pk.classList.add('done');lbl.textContent='Passkey verified.';setTimeout(()=>onOk(),reduced?0:350)}
    catch(e){pk.classList.remove('sweep');lbl.textContent=e instanceof ApiErr?e.message:passkeyWords(e);a.prepared=null}};
  $('[data-pkcancel]',host).onclick=()=>{host.innerHTML='';host.hidden=true;refreshSheet()};
  setTimeout(()=>pk.focus(),60);
}
/* a guard replaces the action you tapped, in place */
function guardHtml(a){
  if(a.level==='confirm')return `<div class="confirm" data-guard>${a.words}${a.channel==='voice'?'<p class="p2">Voice opened this. Confirm it by hand.</p>':''}<div class="acts"><button class="btn primary" data-go>${esc(a.goLabel||a.label)}</button><button class="btn" data-cancel>Cancel</button></div></div>`;
  if(a.level==='hold')return `<div class="confirm" data-guard>${a.words}<button class="hold" data-hold><span class="hf"></span><span>${esc(a.holdLabel)}</span></button><button class="btn quiet" data-cancel>Cancel</button></div>`;
  return `<div class="confirm" data-guard>${a.words}${a.channel==='voice'?'<p class="p2">Voice can only bring you here. Finish this by hand.</p>':''}
    <div class="pk"><button class="g circ press" data-pk aria-label="Touch to use your passkey"><span class="needle" aria-hidden="true"></span><svg class="ic dev" aria-hidden="true"><use href="#i-key"/></svg><svg class="ic tick" aria-hidden="true" style="width:40px;height:40px"><use href="#i-check"/></svg></button><span class="lbl" data-pklbl>${esc(a.pkLabel||'Touch to use your passkey')}</span></div>
    <button class="hold" data-hold disabled><span class="hf"></span><span>${esc(a.holdLabel||'Hold to confirm')}</span></button><button class="btn quiet" data-cancel>Cancel</button></div>`;
}
async function armGuard(a,anchor,ctx){
  ctx=ctx||{};a.channel=ctx.channel||'touch';
  if(a.owner&&!isOwner()){toast('Only the owner can do that.','');return}
  if(a.disabled){toast(a.label+' is not possible right now.'+(a.stillText?' It is '+a.stillText+'.':''),'');return}
  if(a.refused){toast(a.refused,'fail');return}
  if(a.unusable){toast(a.unusable,'fail');return}
  if(a.level==='plain'){run(a);return}
  /* tier 2 and 3 ask the server first: it decides what is allowed and says what will happen */
  if(a.level==='hold'){const d=CAT[a.action];a.consequence=d?[d.description].concat(d.consequences||[]).join(' '):a.label}
  else if(!a.prepared){try{await prepareAction(a,a.channel)}catch(e){toast(e.message,'fail');return}}
  else a.consequence=preparedWords(a.prepared);
  /* the owner's notes on this device, to mark them among the consequences */
  if(isOwner()&&a.target&&DEVINFO[a.target]&&NOTES[a.target]===undefined)await loadNotes(a.target);
  a.words=guardWords(a);
  if(!anchor.isConnected)return;
  const host=anchor.parentElement;const old=$('[data-guard]',host);if(old){old.remove();$$('[data-anchor]',host).forEach(x=>x.hidden=false)}
  anchor.hidden=true;anchor.dataset.anchor='1';
  const wrap=document.createElement('div');wrap.innerHTML=guardHtml(a);const block=wrap.firstElementChild;anchor.after(block);
  if(!reduced)block.animate([{opacity:0,transform:'translateY(8px)'},{opacity:1,transform:'none'}],{duration:450,easing:springEasing()});
  const finish=()=>{if(!block.isConnected)return;block.remove();anchor.hidden=false;delete anchor.dataset.anchor;const f=anchor.matches('button,[tabindex]')?anchor:$('button,[tabindex]',anchor);if(f&&f.isConnected){try{f.focus({preventScroll:true})}catch(e){}}};
  $('[data-cancel]',block).onclick=finish;
  const go=()=>{
    block.innerHTML=`${a.words}<div class="acts"><button class="btn primary" disabled>${PEND}<span>${esc(a.progressText?a.progressText[0].toUpperCase()+a.progressText.slice(1)+'…':'Waiting for the '+(a.deviceName||'device'))}</span></button></div>`;
    const prevDone=a.onDone;
    a.onDone=(rt,v)=>{block.innerHTML=`<div class="okline">${ICON('check')}<span>${esc(LASTDONE[a.key]?LASTDONE[a.key].text:'Done')}</span></div>`;setTimeout(finish,2600);prevDone&&prevDone(rt,v)};
    a.onFail=text=>{block.innerHTML=`${a.words}<div class="failband">${ICON('warn')}<span>${esc(text)}</span><span class="acts"><button class="btn" data-dismiss-g>Dismiss</button></span></div>`;$('[data-dismiss-g]',block).onclick=()=>{finish();dismissFail(a.id)}};
    run(a);
  };
  if(a.level==='confirm'){$('[data-go]',block).onclick=go;$('[data-cancel]',block).focus();return}
  if(a.level==='critical'){
    const pk=$('[data-pk]',block);const lbl=$('[data-pklbl]',block);
    pk.onclick=async()=>{if(pk.classList.contains('sweep')||pk.classList.contains('done'))return;pk.classList.add('sweep');lbl.textContent=a.pkWait||'Checking…';
      try{await stepUp(a);pk.classList.remove('sweep');pk.classList.add('done');lbl.textContent='Passkey verified for this action. Now hold.';const h=$('[data-hold]',block);h.disabled=false;h.focus()}
      catch(e){pk.classList.remove('sweep');lbl.textContent=(e instanceof ApiErr?e.message:passkeyWords(e))}};
    setTimeout(()=>pk.focus(),60);
  }
  wireHold($('[data-hold]',block),()=>{go()});
}
function wireHold(h,done){
  let t=null,down=false;const ms=1500;
  const start=e=>{if(h.disabled||down)return;down=true;h.classList.add('holding');h.style.setProperty('--p',100);try{h.setPointerCapture(e.pointerId)}catch(err){}t=setTimeout(()=>{down=false;h.classList.remove('holding');h.classList.add('done');h.disabled=true;done()},ms)};
  const cancel=()=>{if(!down)return;down=false;clearTimeout(t);h.classList.remove('holding');h.style.setProperty('--p',0)};
  h.addEventListener('pointerdown',e=>{if(e.pointerType==='mouse'&&e.button!==0)return;e.preventDefault();start(e)});
  h.addEventListener('pointerup',cancel);h.addEventListener('pointercancel',cancel);h.addEventListener('pointerleave',cancel);
  h.addEventListener('keydown',e=>{if((e.key===' '||e.key==='Enter')&&!e.repeat){e.preventDefault();start(e)}});
  h.addEventListener('keyup',e=>{if(e.key===' '||e.key==='Enter')cancel()});
}

export {dispatch,doNow,preparedAction,QDONE,qTell,paintQDone,chatTell,pkStep,armGuard};

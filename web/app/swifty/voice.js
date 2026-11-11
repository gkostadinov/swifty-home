import {$,$$,clamp,esc,PEND} from '../core/util.js';
import {api} from '../core/api.js';
import {S,isOwner,refreshSoon,TGT,DEVINFO} from '../core/store.js';
import {cardSheet} from '../cards/index.js';
import {sleep} from '../core/actions.js';
import {toast} from '../ui/toast.js';
import {A} from '../core/catalog.js';
import {doNow,preparedAction,chatTell} from '../core/guards.js';
import {SHEET,openSheet,closeSheet,row} from '../ui/sheets.js';
import {parse,execute} from './parser.js';
import {CHAT,growChat,chatCommand,askSwifty} from './chat.js';
import {SWF} from './figure.js';
import {showLock} from '../ui/lock.js';
import {touched} from '../ui/dock.js';
import {setView} from '../ui/views.js';

/* the microphone. Tap and talk and it stops when you do, or hold it and let go */
const VOICE={on:false,t:null,rec:null,starting:null,busy:false,ctx:null,partial:null,live:'',into:null,held:false,err:null};
/* Safari only lets audio start inside a tap, so this runs in the tap itself and the context stays for next time */
function audioCtx(){const AC=window.AudioContext||window.webkitAudioContext;if(!AC)return null;if(!VOICE.ctx)VOICE.ctx=new AC();if(VOICE.ctx.state==='suspended')VOICE.ctx.resume().catch(()=>{});return VOICE.ctx}
/* only while listening: open the microphone, keep the samples in memory, and hand them over as a WAV */
async function recStart(){const ctx=audioCtx();if(!ctx||!navigator.mediaDevices)throw new Error('no audio');const stream=await navigator.mediaDevices.getUserMedia({audio:{channelCount:1,echoCancellation:true,noiseSuppression:true}});if(!VOICE.on){stream.getTracks().forEach(t=>t.stop());return}
  if(ctx.state!=='running')await Promise.race([ctx.resume(),sleep(500)]);const src=ctx.createMediaStreamSource(stream);const node=ctx.createScriptProcessor(4096,1,1);const rec={stream,ctx,src,node,chunks:[],at:performance.now(),heard:0,floor:0,n:0};node.onaudioprocess=e=>{const d=new Float32Array(e.inputBuffer.getChannelData(0));rec.chunks.push(d);hearing(rec,d)};src.connect(node);node.connect(ctx.destination);VOICE.rec=rec;liveWords()}
/* stops by itself a moment after you do. A held button waits for the hand instead */
function hearing(rec,d){let s=0;for(let i=0;i<d.length;i+=8)s+=d[i]*d[i];const level=Math.sqrt(s/(d.length/8));rec.n++;if(rec.n<=4)rec.floor=rec.n===1?level:Math.min(rec.floor,level);if(rec.n>2&&level>Math.min(.06,Math.max(.012,rec.floor*3)))rec.heard=performance.now();if(VOICE.held||!VOICE.on)return;const now=performance.now();if(rec.heard?now-rec.heard>1400:now-rec.at>8000)stopListening()}
const niceWords=t=>{t=String(t||'').trim();return t&&t===t.toUpperCase()?t.toLowerCase():t};
function showWords(w){const t=niceWords(w.text);if(t&&t!==VOICE.live&&VOICE.rec)VOICE.rec.heard=performance.now();VOICE.live=t;if(VOICE.into==='chat'){const i=$('#chat-in');if(i){i.value=t;growChat(i)}return}const el=$('#voice-words');if(el&&t)el.textContent=t;const h=$('#voice-hint');if(h&&w.to_swifty)h.textContent='This will go to Swifty.'}
/* the words so far, over and over while talking. Each ask covers everything said, so they can correct themselves */
async function liveWords(){while(VOICE.on&&VOICE.rec){await sleep(650);const r=VOICE.rec;if(!VOICE.on||!r||r.chunks.length<8)continue;const asked=api('POST','/voice?words=true',undefined,{raw:wavEncode(r.chunks,r.ctx.sampleRate),type:'audio/wav',quiet:true}).catch(()=>null);VOICE.partial=asked;const w=await asked;VOICE.partial=null;if(w&&VOICE.on)showWords(w)}}
function recClose(){const r=VOICE.rec;VOICE.rec=null;if(!r)return null;try{r.node.onaudioprocess=null;r.node.disconnect();r.src.disconnect();r.stream.getTracks().forEach(t=>t.stop())}catch(e){}return r}
function wavEncode(chunks,rate){const total=chunks.reduce((s,c)=>s+c.length,0);const pcm=new Float32Array(total);let o=0;for(const c of chunks){pcm.set(c,o);o+=c.length}
  const target=rate>16000?16000:rate;const ratio=rate/target;const n=Math.floor(total/ratio);const buf=new ArrayBuffer(44+n*2);const d=new DataView(buf);const str=(p,s)=>{for(let i=0;i<s.length;i++)d.setUint8(p+i,s.charCodeAt(i))};
  str(0,'RIFF');d.setUint32(4,36+n*2,true);str(8,'WAVE');str(12,'fmt ');d.setUint32(16,16,true);d.setUint16(20,1,true);d.setUint16(22,1,true);d.setUint32(24,target,true);d.setUint32(28,target*2,true);d.setUint16(32,2,true);d.setUint16(34,16,true);str(36,'data');d.setUint32(40,n*2,true);
  for(let i=0;i<n;i++){const a=Math.floor(i*ratio),b=Math.max(a+1,Math.floor((i+1)*ratio));let s=0;for(let j=a;j<b&&j<total;j++)s+=pcm[j];s=clamp(s/(b-a),-1,1);d.setInt16(44+i*2,s<0?s*32768:s*32767,true)}return buf}
const capsules=on=>$$('.capsule,.ptt,#chat-mic').forEach(c=>c.classList.toggle('live',on));
function startListening(opts){opts=opts||{};if(VOICE.on||VOICE.busy||S.locked)return;if(opts.into==='chat'&&CHAT.busy){toast('Swifty is still answering.','');return}VOICE.on=true;VOICE.err=null;VOICE.held=!!opts.held;VOICE.into=opts.into||null;VOICE.live='';capsules(true);if(VOICE.into==='chat'){CHAT.hear='on';SWF.sync();const i=$('#chat-in');if(i){i.value='';i.placeholder='Listening…'}}else openSheet('voice',null,{listening:true,replace:SHEET.open});VOICE.t=setTimeout(stopListening,30000);VOICE.starting=recStart().catch(e=>{VOICE.err=e})}
function cancelListening(){VOICE.on=false;VOICE.held=false;clearTimeout(VOICE.t);capsules(false);recClose();const i=$('#chat-in');if(VOICE.into==='chat'){CHAT.hear=null;SWF.sync()}if(VOICE.into==='chat'&&i){i.value='';growChat(i);i.placeholder='Ask Swifty'}}
function voiceSay(answer,reply){openSheet('voice',null,{replace:SHEET.open,answer,reply:reply||{}})}
async function stopListening(){if(!VOICE.on)return;VOICE.on=false;VOICE.held=false;clearTimeout(VOICE.t);capsules(false);VOICE.busy=true;const chat=VOICE.into==='chat';const say=m=>{if(chat)toast(m,'');else voiceSay(m)};if(chat){CHAT.hear='busy';SWF.sync()}else openSheet('voice',null,{working:true,words:VOICE.live,replace:true});
  try{await VOICE.starting;await VOICE.partial;const err=VOICE.err;VOICE.err=null;if(err){say(err.name==='NotAllowedError'?'The microphone is not allowed for this page.':'The microphone could not be opened.');return}
    const r=recClose();const secs=r?r.chunks.reduce((s,c)=>s+c.length,0)/r.ctx.sampleRate:0;if(!r||secs<0.25){say('That was too short.');return}
    const audio={raw:wavEncode(r.chunks,r.ctx.sampleRate),type:'audio/wav'};
    if(chat){const t=niceWords((await api('POST','/voice?words=true',undefined,audio)).text);if(t){if(!chatCommand(t,true))askSwifty(t,{spoken:true})}else say('I did not catch that.');return}
    voiceReply(await api('POST','/voice',undefined,audio))
  }catch(e){if(e.status!==401&&e.status!==423)say(e.message)}finally{recClose();VOICE.busy=false;const i=$('#chat-in');if(chat){CHAT.hear=null;SWF.sync()}if(chat&&i){i.value='';growChat(i);i.placeholder='Ask Swifty'}}}
/* where an action that needs a page is finished: the page of the device it is for */
const whereFor=(action,target)=>/^tv\./.test(action)?'music':/^scene\./.test(action)?'lights':/^pets\./.test(action)?'pets':/^print\./.test(action)?'printer':/^vm\./.test(action)?'comp:'+target:/^(host|router)\./.test(action)&&TGT.host?'host':target&&DEVINFO[target]?cardSheet(target):'host';
function runChoice(c,msg){const a=A({action:c.action,target:c.target,params:c.params||{},cls:'voice',key:c.target,label:c.label,targetName:c.label,deviceName:'device',holdLabel:'Hold to confirm'});if(msg){doNow(a,{channel:'typed',tell:chatTell(msg)});return}if(a.level!=='critical'){closeSheet();doNow(a,{channel:'voice'});return}SHEET.stack=[];openSheet(whereFor(c.action,c.target),null,{replace:true,action:a,channel:'touch'})}
/* what the server's phrases do not cover: this page's parser knows every device by its own name, and Swifty works out the rest */
function voiceFallback(r){const p=parse(r.heard);
  if(p.kind==='query'){voiceSay(p.answer,r);return true}
  if(p.kind!=='none'){execute(r.heard,{channel:'voice',now:true});return true}
  const why=r.why||p.why;if(why){voiceSay(why,r);return true}
  if(S.assistant){askSwifty(r.heard,{spoken:true,command:true});return true}
  return false}
function voiceReply(r){const k=r.kind;if(k==='swifty'){if(r.ask)askSwifty(r.ask,{spoken:true});else openSheet('swifty',null,{replace:SHEET.open});return}if(k==='nothing'&&r.heard&&voiceFallback(r))return;
  if(k==='executed'){closeSheet();refreshSoon();const sim=r.phase==='simulated';const note=!sim&&r.run&&r.run.detail&&(r.run.steps||[]).some(s=>s.phase==='no_response')?' '+r.run.detail:'';toast((r.label||'Done')+(sim?': dry-run, nothing was sent.':'.')+note,sim?'':note?'':'ok');return}
  if(k==='no_response'||k==='failed'){voiceSay((r.label||'That')+': '+((r.run&&r.run.detail)||'the device did not answer.'),r);refreshSoon();return}
  if(k==='prepared'||k==='acknowledgement_required'){closeSheet();doNow(preparedAction(r),{channel:'voice'});return}
  if(k==='nav'){const pages={lights:'lights',music:'music',tv:'music',pets:'pets',camera:'camera',printer:'printer',history:'history',passkeys:'passkeys',monitor:'host',host:'host',technical:'host',health:'host',devices:'devices',integrations:'integrations',more:'more'};const to=pages[r.page];if(to==='rooms'||r.page==='rooms'){closeSheet();setView('rooms');return}if(to&&(isOwner()||!/^(history|passkeys|host|printer|integrations)$/.test(to))){SHEET.stack=[];openSheet(to,null,{replace:SHEET.open});return}voiceSay('That part is not available here.',r);return}
  if(k==='locked'){closeSheet();showLock('Locked. Use your passkey to come back.');return}
  if(k==='help'){voiceSay('Anything in the home, by its name: “dim the bedroom lights”, “turn off the kettle”, “set the AC to 22”, “pause the TV” or “feed the pets”.',r);return}
  voiceSay(r.why||'Not understood. Nothing ran.',r)}
function wireCapsule(el,opts){
  let t=null,down=null,held=false;const mic=e=>opts.holdOnly||(e.target.closest&&e.target.closest('.micwrap'));
  el.addEventListener('pointerdown',e=>{if(e.pointerType==='mouse'&&e.button!==0)return;audioCtx();down=e.pointerId;touched();el.classList.add('held');held=false;if(VOICE.on)return;t=setTimeout(()=>{t=null;held=true;startListening(Object.assign({},opts,{held:true}))},380)});
  const up=e=>{if(down==null||e.pointerId!==down)return;down=null;el.classList.remove('held');if(t){clearTimeout(t);t=null;if(e.type!=='pointerup')return;if(mic(e)){audioCtx();startListening(opts)}else openSheet('more',el)}else if(held){held=false;stopListening()}else if(e.type==='pointerup'&&VOICE.on&&mic(e))stopListening()};
  el.addEventListener('pointerup',up);el.addEventListener('pointercancel',up);el.addEventListener('pointerleave',e=>{if(down!=null&&VOICE.on)up(e)});
  el.addEventListener('keydown',e=>{if(e.key==='Enter'||e.key===' '){e.preventDefault();if(!e.repeat){if(!opts.holdOnly)openSheet('more',el,{focus:'field'});else if(VOICE.on)stopListening();else{audioCtx();startListening(opts)}}}});
}

const voiceSheets={
  voice(arg,opts){
    const meter=`<div class="meter" aria-hidden="true">${Array.from({length:9},(_,i)=>`<i style="--d:${i}"></i>`).join('')}</div>`;
    if(opts.listening)return {title:'Listening',glowHue:270,glowA:0.14,html:`<div class="listen" role="status">${meter}<p class="sent" id="voice-words">Listening…</p><p class="p2" id="voice-hint">Stop talking when you are done, or tap Done. Start with “Swifty” to ask it anything. Nothing is kept.</p><button class="btn primary" id="voice-done">Done</button></div>`,mount(root){$('#voice-done',root).onclick=stopListening}};
    if(opts.working)return {title:'Working on it',glowHue:270,glowA:0.14,html:`<div class="listen" role="status"><p class="sent">${opts.words?esc(opts.words):PEND+' Working out what you said.'}</p></div>`};
    const r=opts.reply||{};const choices=r.choices||[];
    const html=`${r.heard?`<p class="p2">Heard: “${esc(r.heard)}”</p>`:''}<div class="answer">${esc(opts.answer||'Nothing to do.')}</div>${choices.length?`<div class="rows">${choices.map((c,i)=>row(null,esc(c.label),null,null,`data-choice="${i}"`)).join('')}</div>`:''}<input class="field" id="vq" placeholder="Or type it instead" aria-label="Type it instead" autocomplete="off">`;
    return {title:'What was heard',html,glowHue:270,glowA:0.14,mount(root){
      $$('[data-choice]',root).forEach(b=>b.onclick=()=>runChoice(choices[+b.dataset.choice]));
      const vq=$('#vq',root);vq.addEventListener('keydown',e=>{if(e.key==='Enter'){e.preventDefault();const ok=execute(vq.value,{channel:'typed',now:true});if(ok===false){const p=parse(vq.value);$('.answer',root).textContent=p.kind==='query'?p.answer:(p.why||'Not understood. Try “dim the bedroom lights”, “volume 20” or “feed the pets”.')}}})}};
  }
};

export {VOICE,recClose,capsules,startListening,cancelListening,stopListening,whereFor,runChoice,voiceReply,wireCapsule,voiceSheets};

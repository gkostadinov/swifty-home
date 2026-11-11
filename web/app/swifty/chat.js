import {$,$$,esc,ICON,PEND} from '../core/util.js';
import {api,enc} from '../core/api.js';
import {VMS,S,isOwner,D,refreshSoon} from '../core/store.js';
import {bulbName,acWord} from '../pages/home.js';
import {sleep} from '../core/actions.js';
import {toast} from '../ui/toast.js';
import {doNow,preparedAction,chatTell} from '../core/guards.js';
import {SHEET,openSheet,roomOf} from '../ui/sheets.js';
import {said,spokenForm,parse,execute} from './parser.js';
import {VOICE,runChoice,voiceReply,wireCapsule} from './voice.js';
import {SWF} from './figure.js';

/* Swifty, the home's own model. The conversation lives in this page and goes when it closes */
const CHAT={msgs:[],busy:false,job:null,hear:null,id:Array.from(crypto.getRandomValues(new Uint8Array(8)),x=>x.toString(16).padStart(2,'0')).join('')};
/* while the microphone is held from inside the chat, the chat stays and shows it: 'on' while listening, 'busy' while the words are worked out */
const chatUp=()=>SHEET.open&&SHEET.type==='swifty';
/* true while the newest answer is the owner's agent running commands on a machine: the chat and the figure dress for it. A turn that has only just begun does not undo it */
const hacking=()=>{for(let i=CHAT.msgs.length-1;i>=0;i--){const m=CHAT.msgs[i],t=m.tools||[];if(m.who!=='ai'||m.live&&!t.length&&!m.text)continue;return t.some(x=>/^((ba|z)?sh$|ssh|shell|exec|run|cmd|command|terminal)/i.test(x.name||''))}return false};
/* what the newest answer says of the weather, when the question before it asked about it: 'rain', 'snow' or 'sun', whichever it names first. "No rain" is not rain */
const weather=()=>{for(let i=CHAT.msgs.length-1;i>0;i--){const m=CHAT.msgs[i],q=CHAT.msgs[i-1];if(m.who!=='ai'||m.live&&!m.text)continue;
  if(m.fail||q.who!=='me'||!/weather|forecast|rain|snow|sunny?\b|sunshine|umbrella|temperature|degrees|outside/i.test(q.text))return null;
  const at=r=>{const k=m.text.search(r);return k<0||/\b(no|not|without|any|never)\s+(\w+\s+)?$/i.test(m.text.slice(Math.max(0,k-18),k))?1e9:k},s=at(/snow|sleet|blizzard|flurr/i),r=at(/rain|drizzl|shower|thunder|storm|downpour/i),u=at(/sunny|sunshine|clear sk|\bsun\b|\bclear\b/i),b=Math.min(s,r,u);
  return b>1e8?null:b===s?'snow':b===r?'rain':'sun'}return null};
const webLink=u=>{try{return ['https:','http:'].includes(new URL(u).protocol)}catch(e){return false}};
/* a little of the markdown the agent writes. The text is escaped first, so only these three become tags */
const mdLite=s=>esc(s).replace(/\*\*([^*\n]+)\*\*/g,'<b>$1</b>').replace(/`([^`\n]+)`/g,'<code>$1</code>').replace(/\[([^\]\n]+)\]\(([^)\s]+)\)/g,(m,t,u)=>webLink(u.replace(/&amp;/g,'&'))?`<a href="${u}" target="_blank" rel="noopener noreferrer">${t}</a>`:m);

function homeNow(){const when=new Date().toLocaleString('en-GB',{weekday:'long',day:'numeric',month:'long',year:'numeric',hour:'2-digit',minute:'2-digit'});
  const lights=D.bulbs.filter(b=>b.known).map(b=>bulbName(b)+' ('+(roomOf('bulb'+b.id)||'no room')+') '+(b.on?'on at '+b.bri+'%':'off'));const sockets=D.plugs.filter(p=>p.known).map(p=>p.name+' '+(p.on?'on':'off'));const coolers=D.acs.filter(c=>c.known).map(c=>c.name+' '+acWord(c)+(c.indoor!=null?', '+c.indoor+'° inside':''));const machines=isOwner()?VMS.filter(v=>v.known).map(v=>v.name+' '+v.state):[];
  return ['It is '+when+'.',lights.length?'Lights: '+lights.join('; ')+'.':'',sockets.length?'Sockets: '+sockets.join('; ')+'.':'',coolers.length?'Air conditioning: '+coolers.join('; ')+'.':'',D.speaker.known?'Speaker volume '+D.speaker.vol+(D.speaker.mute?', muted':'')+'.':'',D.tv.known?'TV '+(D.tv.on?'on, '+D.tv.input:'off')+'.':'',machines.length?'Computers: '+machines.join('; ')+'.':''].filter(Boolean).join(' ').slice(0,3000)}
const toolLine=t=>`<div>${t.state==='running'?PEND:ICON(t.state==='done'?'check':t.state==='info'?'clock':'x')}<span>${esc(t.label)}</span></div>`;
/* the person's own lines (an answer, a note) stay where they happened among the agent's */
const toolsOf=m=>{const t=m.tools||[],n=m.notes||[];if(!n.length)return t;const out=[];for(let i=0;i<=t.length;i++){for(const x of n)if(Math.min(x.at||0,t.length)===i)out.push(x);if(i<t.length)out.push(t[i])}return out};
const growChat=i=>{i.style.height='48px';i.style.height=Math.min(i.scrollHeight,140)+'px';const s=$('#swf');if(s)s.style.bottom=(i.offsetHeight+8)+'px'};
/* what the agent stopped to ask. One choice on one question answers at once; anything more waits for Answer */
function askHtml(q){return `<div class="ask">${q.items.map((it,i)=>`<div class="q" data-q="${i}">${it.header?`<small>${esc(it.header)}</small>`:''}<b>${esc(it.question)}</b>${it.detail?`<p>${esc(it.detail)}</p>`:''}${(it.options||[]).length?`<div class="opts">${it.options.map((o,j)=>`<button class="opt" data-o="${j}">${esc(o.label)}${o.description?`<small>${esc(o.description)}</small>`:''}</button>`).join('')}</div>`:''}<input class="field" data-custom="1" placeholder="Or type your answer" aria-label="Your own answer" autocomplete="off"></div>`).join('')}<div class="askbar"><button class="pill" data-ask-skip="1">Skip</button><button class="btn primary" data-ask-send="1">Answer</button></div></div>`}
function pickOption(b){const m=CHAT.msgs[CHAT.msgs.length-1];const q=m&&m.question;if(!q)return;const block=b.closest('.q');const it=q.items[+block.dataset.q];if(it.multiSelect){b.classList.toggle('sel');return}$$('.opt',block).forEach(x=>x.classList.toggle('sel',x===b));if(q.items.length===1)sendAnswer(false)}
async function sendAnswer(skip){const m=CHAT.msgs[CHAT.msgs.length-1];const q=m&&m.question;const card=$('#chat .ask');if(!q||!card||!CHAT.job||card.classList.contains('sending'))return;const answers=[],said=[];
  if(!skip)for(const b of $$('.q',card)){const it=q.items[+b.dataset.q];const selected=$$('.opt.sel',b).map(o=>it.options[+o.dataset.o].label);const custom=$('[data-custom]',b).value.trim();if(!selected.length&&!custom){toast('Answer each question, or skip.','');return}answers.push(custom?{id:it.id,selected,custom}:{id:it.id,selected});said.push(selected.concat(custom?[custom]:[]).join(', '))}
  card.classList.add('sending');try{await api('POST','/assistant/jobs/'+enc(CHAT.job)+'/answer',{call_id:q.call_id,answers,skip});m.notes=(m.notes||[]).concat([{id:'a'+q.call_id,label:skip?'Skipped the question':said.join(' · '),state:'done',at:(m.tools||[]).length}]);m.question=null;paintLive(m)}catch(e){card.classList.remove('sending');if(e.status!==401&&e.status!==423)toast(e.message,'fail')}}
function stopChat(){if(!CHAT.job||CHAT.stopping)return;CHAT.stopping=true;paintChatBar();api('POST','/assistant/jobs/'+enc(CHAT.job)+'/stop',{}).catch(e=>{CHAT.stopping=false;paintChatBar();if(e.status!==401&&e.status!==423)toast(e.message,'fail')})}
/* the send button turns into Stop for as long as there is something to stop */
function paintChatBar(){const b=$('#chat-send');if(!b)return;const stop=!!CHAT.job;b.innerHTML=ICON(stop?'stop':'send');b.setAttribute('aria-label',stop?'Stop':'Send');b.disabled=!!CHAT.stopping||(CHAT.busy&&!stop)}
function msgHtml(m,i){if(m.live)return `<div class="msg ai" data-live="1"><div class="tools"${toolsOf(m).length?'':' hidden'}>${toolsOf(m).map(toolLine).join('')}</div><div class="body typing"${m.text?'':' hidden'}>${mdLite(m.text)}</div><div class="askslot" data-v="${m.question?esc(m.question.call_id):''}"${m.question?'':' hidden'}>${m.question?askHtml(m.question):''}</div><span class="wait"${m.text||m.question?' hidden':''}>${PEND}</span></div>`;
  return `<div class="msg ${m.who}${m.fail?' fail':''}">${toolsOf(m).length?`<div class="tools">${toolsOf(m).map(toolLine).join('')}</div>`:''}${m.text?`<div class="body">${m.who==='ai'?mdLite(m.text):esc(m.text)}</div>`:''}${(m.choices||[]).map((c,j)=>`<button class="pill" data-chat-choice="${i}:${j}">${esc(c.label)}</button>`).join('')}${(m.mem||[]).map((x,j)=>`<div class="memline">${ICON(x.gone?'x':'check')}<span>${x.gone?'Forgotten':'Remembered'}: ${esc(x.text)}</span>${x.gone?'':`<button class="pill" data-forget="${i}:${j}">Undo</button>`}</div>`).join('')}</div>`}
/* while an answer is being written only its words change, so Stop stays still under the finger */
function paintLive(m){const chat=$('#chat');const b=chat&&chat.lastElementChild;if(!b||!b.dataset.live){paintChat();return}const sb=$('#sbody');const atEnd=!!sb&&sb.scrollHeight-sb.scrollTop-sb.clientHeight<80;const t=$('.tools',b),y=$('.body',b),w=$('.wait',b),a=$('.askslot',b);const tools=toolsOf(m).map(toolLine).join('');if(t.dataset.v!==tools){t.dataset.v=tools;t.innerHTML=tools;t.hidden=!tools}if(m.text){const h=mdLite(m.text);if(y.dataset.v!==h){y.dataset.v=h;y.innerHTML=h;y.hidden=false}}const call=m.question?m.question.call_id:'';if(a.dataset.v!==call){a.dataset.v=call;a.innerHTML=m.question?askHtml(m.question):'';a.hidden=!m.question}w.hidden=!!(m.text||m.question);if(sb&&(atEnd||m.question))sb.scrollTop=sb.scrollHeight;chat.classList.toggle('hack',hacking());SWF.sync()}
function paintChat(){const el=$('#chat');if(!el)return;
  const none=!CHAT.msgs.length;el.classList.toggle('lone',none);el.classList.toggle('hack',hacking());
  el.innerHTML=(none?'<div class="swhero" id="swf-hero"></div><p class="quiet">Ask me to dim a room, what the weather is tomorrow, or to search for something.</p>':'')+CHAT.msgs.map(msgHtml).join('')+(CHAT.busy&&!CHAT.job?`<div class="msg ai">${PEND}</div>`:'');
  const sb=$('#sbody');if(sb)sb.scrollTop=sb.scrollHeight;paintChatBar();SWF.sync()}
/* an answer still being written that this page lost sight of: after a reload, or after the service restarted */
async function resumeChat(note){if(!S.assistant||CHAT.job||CHAT.busy)return false;let r;try{r=await api('POST','/assistant/resume',{chat:CHAT.id},{quiet:true})}catch(e){return false}if(!r||!r.job||CHAT.job||CHAT.busy)return false;CHAT.busy=true;followJob(r.job,note);return true}
async function followJob(id,note){const msg={who:'ai',text:'',tools:[],choices:[],live:true,notes:note?[{id:'n',label:note,state:'info',at:0}]:[]};CHAT.msgs.push(msg);CHAT.job=id;paintChat();let missed=0,seen=0;
  for(;;){let j;try{j=await api('GET','/assistant/jobs/'+enc(id)+'?seen='+seen,undefined,{quiet:true});missed=0}catch(e){if(e.status===404||++missed>20){const back=await api('POST','/assistant/resume',{chat:CHAT.id},{quiet:true}).catch(()=>null);if(back&&back.job&&back.job!==id){id=back.job;CHAT.job=id;seen=0;missed=0;continue}msg.tools=msg.tools.map(t=>t.state==='running'?Object.assign({},t,{state:'cut'}):t);msg.text=(msg.text?msg.text+'\n':'')+'Cut off by a service restart. It may still have finished.';msg.fail=!msg.text.includes('\n');break}await sleep(1000);continue}
    seen=j.version;msg.text=j.text||'';msg.tools=j.tools||[];msg.question=j.question||null;if(j.done){if(j.error){msg.text=(msg.text?msg.text+'\n':'')+j.error;msg.fail=!j.text;msg.mood='sorry'}else if(!msg.text){msg.text=msg.tools.length?'Done.':'I do not have an answer for that.';if(!msg.tools.length)msg.mood='confused'}break}paintLive(msg);await sleep(40)}
  msg.live=false;msg.question=null;CHAT.job=null;CHAT.busy=false;CHAT.stopping=false;paintChat();refreshSoon()}
/* in Swifty's chat a plain command runs at once, as it does in the search, and its line turns into a tick. A question, or anything the page does not know, goes to Swifty */
const ASKING=/^(?:how|what|whats|what's|why|when|who|where|which|is|are|do|does|did|can|could|should|would|will|tell)\b/;
function chatCommand(text,spoken){const t=String(text||'').trim();if(!t||CHAT.busy||CHAT.job)return false;
  const plain=spokenForm(t.toLowerCase()).replace(/^(?:please |(?:can|could|would|will) you (?:please )?)/,'');if(/\?\s*$/.test(t)||ASKING.test(plain))return false;
  const p=parse(t);if(!['action','multi','scene'].includes(p.kind))return false;
  if(p.kind==='action'&&(p.a.level==='critical'||p.a.disabled||(p.a.owner&&!isOwner())))return false;
  const msg={who:'ai',text:'',choices:[]};CHAT.msgs.push({who:'me',text:t},msg);paintChat();
  if(execute(t,{channel:spoken?'voice':'typed',now:true,stay:true,tell:chatTell(msg)})===false){CHAT.msgs.splice(CHAT.msgs.indexOf(msg),1);paintChat()}
  return true}
async function askSwifty(text,opts){text=String(text||'').trim();if(!text||CHAT.busy)return;CHAT.msgs.push({who:'me',text});CHAT.busy=true;if(SHEET.type==='swifty'&&SHEET.open)paintChat();else openSheet('swifty',null,{replace:SHEET.open});
  const messages=CHAT.msgs.filter(m=>!m.fail&&(m.who==='me'||m.text)).slice(-10).map(m=>({role:m.who==='me'?'user':'assistant',text:m.text.slice(0,2000)}));let r;
  try{r=await api('POST','/assistant',{messages,context:homeNow(),chat:CHAT.id,spoken:!!(opts&&opts.spoken),command:!!(opts&&opts.command)})}catch(e){CHAT.busy=false;if(e.code==='assistant_busy'&&await resumeChat('Still finishing your last request. Ask again when it is done.'))return;if(e.status!==401&&e.status!==423){CHAT.msgs.push({who:'ai',text:e.message,fail:true});paintChat()}return}
  if(r.job){followJob(r.job);return}
  const msg={who:'ai',text:r.say||'',choices:[],mem:(r.memory||[]).map(m=>({id:m.id,text:m.text,gone:false}))};const later=[],now=[];
  for(const x of r.results||[]){if(x.kind==='executed'){msg.mood=msg.mood||'pleased';refreshSoon();continue}if(x.kind==='choices'){msg.choices=x.choices||[];continue}if(x.kind==='prepared'||x.kind==='acknowledgement_required'){now.push(x);continue}if(['nav','locked'].includes(x.kind)){later.push(x);continue}const why=x.why||(x.run&&x.run.detail);msg.mood=/^(failed|no_response)$/.test(x.kind)||msg.mood==='sorry'?'sorry':'confused';if(why&&!msg.text.includes(why))msg.text+=(msg.text?'\n':'')+why}
  CHAT.busy=false;CHAT.msgs.push(msg);now.forEach(x=>doNow(preparedAction(x),{channel:'typed',tell:chatTell(msg)}));paintChat();later.forEach(voiceReply)}
const chatSheets={
  swifty(){
    const html=`<div class="chat" id="chat" role="log" aria-live="polite"></div><div class="swf" id="swf" aria-hidden="true"></div><div class="chatbar"><textarea class="field" id="chat-in" rows="1" placeholder="Ask Swifty" aria-label="Ask Swifty" autocomplete="off"></textarea><button class="rk" id="chat-mic" aria-label="Talk to Swifty">${ICON('mic')}</button><button class="rk send" id="chat-send" aria-label="Send">${ICON('send')}</button></div>`;
    return {title:'Swifty',html,glowHue:270,glowA:0.14,mount(root){
      const inp=$('#chat-in',root);const send=()=>{if(CHAT.job){stopChat();return}const t=inp.value.trim();if(!t||CHAT.busy)return;inp.value='';growChat(inp);if(chatCommand(t,false))return;askSwifty(t)};
      $('#chat-send',root).onclick=send;inp.addEventListener('keydown',e=>{if(e.key==='Enter'&&!e.shiftKey&&!e.isComposing){e.preventDefault();send()}});inp.addEventListener('input',()=>growChat(inp));
      wireCapsule($('#chat-mic',root),{holdOnly:true,into:'chat'});
      $('#chat',root).addEventListener('click',e=>{const fg=e.target.closest('[data-forget]');if(fg){const [i,j]=fg.dataset.forget.split(':').map(Number);const x=CHAT.msgs[i]&&(CHAT.msgs[i].mem||[])[j];if(!x||x.gone)return;fg.disabled=true;api('DELETE','/memory/'+enc(x.id)).then(()=>{x.gone=true;paintChat()}).catch(err=>{fg.disabled=false;if(err.status!==401&&err.status!==423)toast(err.message,'fail')});return}const o=e.target.closest('.ask .opt');if(o){pickOption(o);return}if(e.target.closest('[data-ask-send]')){sendAnswer(false);return}if(e.target.closest('[data-ask-skip]')){sendAnswer(true);return}const b=e.target.closest('[data-chat-choice]');if(!b)return;const [i,j]=b.dataset.chatChoice.split(':').map(Number);runChoice(CHAT.msgs[i].choices[j],CHAT.msgs[i])});
      if(!VOICE.on&&!VOICE.busy)CHAT.hear=null;SWF.mount(root);paintChat();resumeChat()}};
  },
};

export {CHAT,chatUp,hacking,weather,toolLine,growChat,paintChat,chatCommand,askSwifty,chatSheets};

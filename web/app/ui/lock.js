import {$,$$,esc,reduced,isPhone} from '../core/util.js';
import {ApiErr,api,passkeyGet,passkeyCreate,passkeyWords} from '../core/api.js';
import {S,refreshState,loadHealth,loadAll} from '../core/store.js';
import {pollSession} from '../core/session.js';
import {startEvents,stopEvents} from '../core/events.js';
import {render} from '../pages/home.js';
import {SHEET,closeSheet} from './sheets.js';
import {VOICE,recClose,capsules} from '../swifty/voice.js';
import {SWF} from '../swifty/figure.js';
import {exitWall} from './dock.js';

/* ============================================================ lock: the whole page frosts, a passkey clears it */
/* the sign-in prompt in progress, so it can be taken down when the session turns out to be fine */
let SIGNIN=null;
function showLock(msg){
  const fresh=!S.locked||$('#lock').hidden;S.locked=true;stopEvents();closeSheet();if(VOICE.on){VOICE.on=false;clearTimeout(VOICE.t);capsules(false);recClose()}
  const l=$('#lock');if(fresh){l.hidden=false;l.classList.add('clear');requestAnimationFrame(()=>requestAnimationFrame(()=>l.classList.remove('clear')))}
  $('#app').inert=true;$('#cap-bottom').inert=true;SHEET.el.inert=true;$('#toasts').inert=true;if(S.wall)exitWall(true);
  $('#locktext').firstChild.textContent=S.signedIn?'Unlock':'Sign in';$('#unlockbtn').setAttribute('aria-label',S.signedIn?'Unlock with your passkey':'Sign in with your passkey');
  $('#locksub').textContent=msg||'Touch to use your passkey.';$('#lockwho').innerHTML='';$('#lockdevs').innerHTML='';
  setTimeout(()=>{if(S.locked)$('#unlockbtn').focus()},50);SWF.home();
}
function clearLock(){if(SIGNIN){SIGNIN.abort();SIGNIN=null}const l=$('#lock');S.locked=false;l.classList.add('clear');$('#app').inert=false;$('#cap-bottom').inert=false;SHEET.el.inert=false;$('#toasts').inert=false;setTimeout(()=>{if(!S.locked)l.hidden=true},reduced?0:500);render()}
function lock(msg){if(S.locked)return;showLock(msg||'Locked. Use your passkey to come back.');api('POST','/session/lock',{},{quiet:true}).catch(()=>{})}
function signOut(){api('POST','/auth/logout',{},{quiet:true}).catch(()=>{}).then(()=>{S.signedIn=false;S.session=null;showLock('Signed out. Use a passkey to come back.')})}
let polling=false;
function startPolling(){if(polling)return;polling=true;setInterval(()=>{if(!document.hidden)refreshState()},4000);setInterval(()=>{if(!document.hidden)pollSession()},20000);setInterval(()=>{if(!document.hidden&&S.signedIn&&!S.locked)loadHealth().then(render)},60000)}
async function enterSession(){S.signedIn=true;await loadAll();clearLock();startPolling();startEvents();const first=isPhone()?$('#t-lights'):$('#cap-top');if(first){try{first.focus({preventScroll:true})}catch(e){}}}
function formHtml(rows,label){return `<div style="display:flex;flex-direction:column;gap:10px;width:min(320px,80vw);margin-top:14px">${rows.join('')}<button class="btn primary" id="lk-go">${esc(label)}</button></div>`}
const lockSay=e=>{$('#locksub').textContent=e instanceof ApiErr?e.message:passkeyWords(e)};
function showEnroll(){$('#locktext').firstChild.textContent='Set up';$('#locksub').textContent='No passkey yet. Enter the one-time token from the server.';
  $('#lockdevs').innerHTML=formHtml(['<input class="field" id="lk-token" placeholder="One-time token" aria-label="One-time token" autocomplete="off" autocapitalize="off" spellcheck="false">','<input class="field" id="lk-name" placeholder="A name for this passkey" aria-label="Passkey name" maxlength="64" autocomplete="off">'],'Add this passkey');
  $('#lk-go').onclick=async()=>{const token=$('#lk-token').value.trim(),name=$('#lk-name').value.trim();if(!token||!name){$('#locksub').textContent='Enter the token and a name first.';return}const b=$('#lk-go');b.disabled=true;
    try{const st=await api('POST','/auth/enroll/start',{token,name},{quiet:true});const credential=await passkeyCreate(st.options);await api('POST','/auth/enroll/finish',{ceremony_id:st.ceremony_id,credential},{quiet:true});$('#lockdevs').innerHTML='';$('#locktext').firstChild.textContent='Sign in';$('#locksub').textContent='Passkey added. Touch to sign in with it.'}catch(e){lockSay(e);b.disabled=false}};
  setTimeout(()=>$('#lk-token').focus(),60)}
/* a link that carries a one-time token goes straight to making the passkey: `#invite=` joins someone the owner invited, `#enroll=` is the first passkey of a new hub (`swifty init` prints that link). Both ask for a name, then for a touch */
const LINKS={
  invite:{word:'Join',say:'You were invited. Name this passkey, then touch to create it.',done:'Done. Touch to sign in with your new passkey.',
    start:body=>api('POST','/auth/invite/start',body,{quiet:true}),finish:body=>api('POST','/auth/invite/finish',body,{quiet:true})},
  enroll:{word:'Set up',say:'This hub has no passkey yet. Name this one, then touch to create it. It will be the owner’s.',done:'Passkey added. Touch to sign in with it.',
    start:body=>api('POST','/auth/enroll/start',body,{quiet:true}),finish:body=>api('POST','/auth/enroll/finish',body,{quiet:true})}};
function showJoin(token,kind){const L=LINKS[kind]||LINKS.invite;$('#locktext').firstChild.textContent=L.word;$('#locksub').textContent=L.say;
  $('#lockdevs').innerHTML=formHtml(['<input class="field" id="lk-name" placeholder="A name for this passkey" aria-label="Passkey name" maxlength="64" autocomplete="off">'],'Create my passkey');
  $('#lk-go').onclick=async()=>{const name=$('#lk-name').value.trim();if(!name){$('#locksub').textContent='Give it a name first.';return}const b=$('#lk-go');b.disabled=true;
    try{const st=await L.start({token,name});const credential=await passkeyCreate(st.options);await L.finish({ceremony_id:st.ceremony_id,credential});history.replaceState(null,'',location.pathname);$('#lockdevs').innerHTML='';$('#locktext').firstChild.textContent='Sign in';$('#locksub').textContent=L.done}catch(e){lockSay(e);b.disabled=false}};
  setTimeout(()=>$('#lk-name').focus(),60)}
async function unlock(){
  const b=$('#unlockbtn');if(b.classList.contains('sweep'))return;b.classList.add('sweep');SWF.check(true);$('#locksub').textContent='Checking…';
  const ask=new AbortController();SIGNIN=ask;
  try{const st=await api('POST','/auth/login/start',{},{quiet:true});const assertion=await passkeyGet(st.options,ask.signal);const r=await api('POST','/auth/login/finish',{ceremony_id:st.ceremony_id,assertion},{quiet:true});S.session=r.session;b.classList.remove('sweep');await SWF.welcome();await enterSession();SWF.arrive()}
  catch(e){b.classList.remove('sweep');SWF.check(false);if(ask.signal.aborted||!S.locked)return;if(e.code==='no_credentials'){showEnroll();return}lockSay(e)}
  finally{if(SIGNIN===ask)SIGNIN=null}
}
$('#unlockbtn').onclick=unlock;
async function boot(){
  const link=location.hash.match(/^#(invite|enroll)=(.+)$/);showLock('Touch to use your passkey.');
  if(link){showJoin(decodeURIComponent(link[2]),link[1]);return}
  /* the passkey button waits until it is known whether this browser is already signed in */
  const b=$('#unlockbtn');b.disabled=true;$('#locksub').textContent='Checking…';
  try{const r=await api('GET','/session',undefined,{quiet:true});S.session=r.session;S.live=!!r.live;S.signedIn=true;if(r.session.locked){showLock('Locked. Use your passkey to come back.');startPolling();return}await enterSession()}
  catch(e){$('#locksub').textContent=e.status!==401?e.message:'Touch to use your passkey.';SWF.home()}
  finally{b.disabled=false}
}
/* while locked, Tab stays inside the lock screen */
addEventListener('keydown',e=>{if(!S.locked||e.key!=='Tab')return;const f=$$('button:not([disabled]),input',$('#lock')).filter(x=>x.offsetParent!==null);if(!f.length)return;const i=f.indexOf(document.activeElement);if(e.shiftKey&&(i<=0)){e.preventDefault();f[f.length-1].focus()}else if(!e.shiftKey&&(i===-1||i===f.length-1)){e.preventDefault();f[0].focus()}});

export {showLock,lock,signOut,boot};

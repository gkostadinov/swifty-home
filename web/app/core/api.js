import {sessionGone} from './session.js';
import {status} from './status.js';
import {showLock} from '../ui/lock.js';

/* ============================================================ the API: every reading and every action goes through here */
/* every path the page asks for is below this one prefix, and only this file names it */
const API_BASE='/api/v1';
class ApiErr extends Error{constructor(status,code,message,detail){super(message||'That did not work.');this.status=status;this.code=code||'';this.detail=detail||null}}
async function api(method,path,body,opts){
  opts=opts||{};const init={method,credentials:'same-origin',cache:'no-store',headers:{}};
  if(opts.raw){init.body=opts.raw;init.headers['Content-Type']=opts.type}
  else if(body!==undefined){init.body=JSON.stringify(body);init.headers['Content-Type']='application/json'}
  let res;try{res=await fetch(API_BASE+path,init)}catch(e){throw new ApiErr(0,'offline','This page could not reach the service.')}
  let data=null;try{data=await res.json()}catch(e){}
  if(res.ok){const out=data||{};out._status=res.status;return out}
  const err=(data&&data.error)||{};
  if(!opts.quiet){if(res.status===401)sessionGone();else if(res.status===423)showLock('Locked. Use your passkey to come back.')}
  throw new ApiErr(res.status,err.code,err.message,err.detail);
}
const enc=s=>encodeURIComponent(s);
/* the full address of a path below the prefix, for what is not a fetch: the event stream */
const apiUrl=path=>API_BASE+path;
/* passkeys: the server's options are base64url text, the browser wants bytes, and the answer goes back as base64url */
const b64u={enc(buf){const b=new Uint8Array(buf);let s='';for(let i=0;i<b.length;i++)s+=String.fromCharCode(b[i]);return btoa(s).replace(/\+/g,'-').replace(/\//g,'_').replace(/=+$/,'')},
  dec(t){const n=String(t).replace(/-/g,'+').replace(/_/g,'/');const s=atob(n+'==='.slice((n.length+3)%4));const a=new Uint8Array(s.length);for(let i=0;i<s.length;i++)a[i]=s.charCodeAt(i);return a.buffer}};
function requestOptions(o){const p=Object.assign({},o.publicKey);p.challenge=b64u.dec(p.challenge);if(p.allowCredentials)p.allowCredentials=p.allowCredentials.map(c=>Object.assign({},c,{id:b64u.dec(c.id)}));return {publicKey:p}}
function creationOptions(o,prefer){const p=Object.assign({},o.publicKey);p.challenge=b64u.dec(p.challenge);p.user=Object.assign({},p.user,{id:b64u.dec(p.user.id)});if(p.excludeCredentials)p.excludeCredentials=p.excludeCredentials.map(c=>Object.assign({},c,{id:b64u.dec(c.id)}));if(prefer==='security-key'){p.authenticatorSelection=Object.assign({},p.authenticatorSelection,{authenticatorAttachment:'cross-platform'});p.hints=['security-key']}return {publicKey:p}}
const extensionsOf=c=>{try{return c.getClientExtensionResults()||{}}catch(e){return {}}};
function assertionJson(c){const r=c.response;return {id:c.id,rawId:b64u.enc(c.rawId),type:c.type,extensions:extensionsOf(c),response:{authenticatorData:b64u.enc(r.authenticatorData),clientDataJSON:b64u.enc(r.clientDataJSON),signature:b64u.enc(r.signature),userHandle:r.userHandle?b64u.enc(r.userHandle):null}}}
function attestationJson(c){const r=c.response;const out={id:c.id,rawId:b64u.enc(c.rawId),type:c.type,extensions:extensionsOf(c),response:{attestationObject:b64u.enc(r.attestationObject),clientDataJSON:b64u.enc(r.clientDataJSON)}};try{if(r.getTransports)out.response.transports=r.getTransports()}catch(e){}return out}
async function passkeyGet(options,signal){if(!window.PublicKeyCredential)throw new ApiErr(0,'no_webauthn','This browser cannot use passkeys here.');const o=requestOptions(options);if(signal)o.signal=signal;return assertionJson(await navigator.credentials.get(o))}
async function passkeyCreate(options,prefer){if(!window.PublicKeyCredential)throw new ApiErr(0,'no_webauthn','This browser cannot use passkeys here.');return attestationJson(await navigator.credentials.create(creationOptions(options,prefer)))}
const passkeyWords=(e,key)=>{const n=e&&e.name;if(key){if(n==='NotAllowedError')return 'The security key was cancelled or timed out. It needs a PIN set on it, because this console asks for one every time.';if(n==='InvalidStateError')return 'That security key is already added here.';if(n==='ConstraintError'||n==='NotSupportedError')return "This security key can't check a PIN or fingerprint, which this console requires."}return n==='NotAllowedError'?'The passkey was cancelled or timed out.':n==='InvalidStateError'?'This device already has a passkey here.':(e&&e.message)||'The passkey did not work.'};

export {ApiErr,api,apiUrl,enc,passkeyGet,passkeyCreate,passkeyWords};

import {T,TGT,stateOf,reasonOf,usable,DEVINFO,NR} from '../core/store.js';
import {deviceSheet} from './generic.js';

/* ============================================================ a router: whether its tunnel for remote access is up. Restarting it is in its actions */
const routerSheets={
  router(t,opts){t=t||TGT.router;
    const lead={word(){if(!usable(t))return (DEVINFO[t]||{}).reason||'Not available';const st=stateOf(t);if(!st)return reasonOf(t)||NR;return st.wireguard_up?'Remote access is up':'Remote access is down'},
      why(){return t===TGT.router&&T.wgKnown?'Restarting it drops every remote session for a moment, this page too if it is opened from outside.':''}};
    return deviceSheet(t,opts,lead);
  },
};

export {routerSheets};

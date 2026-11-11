import {stateOf,reasonOf,usable,DEVINFO,classOf,isOwner,NR} from '../core/store.js';
import {row} from '../ui/sheets.js';
import {deviceSheet} from './generic.js';

/* ============================================================ services and bridges: a server or a hub that answers, or does not */
const serviceSheets={
  service(t,opts){
    const lead={word(){if(!usable(t))return (DEVINFO[t]||{}).reason||'Not available';const st=stateOf(t);if(!st)return reasonOf(t)||NR;return (st.reachable||st.streaming)?'Answering':'Not answering'},
      why(){return stateOf(t)?'This checks that it answers. It sends nothing else.':''},
      /* a bridge carries lights: the owner finds them from here */
      html(){return classOf(t)==='bridge'&&isOwner()?`<div class="rows">${row('plus','Find new lights','The lights this bridge and the network offer',null,'data-open="findlights"')}</div>`:''}};
    return deviceSheet(t,opts,lead);
  },
};

export {serviceSheets};

// jsp-raknet hardcodes protocol 10 in its otherwise compatible open request.
// Configure the actual offline negotiation packet for Pumpkin's protocol 11.
const { Client } = require('jsp-raknet');
const OpenConnectionRequest1 = require('jsp-raknet/js/protocol/OpenConnectionRequest1').default;
Client.prototype.sendConnectionRequest = function sendConnectionRequest() {
  const request = new OpenConnectionRequest1();
  request.mtuSize = this.mtuSize;
  request.protocol = 11;
  request.encode();
  this.sendBuffer(request.buffer);
  this.emit('connecting', { mtuSize: request.mtuSize, protocol: request.protocol });
};

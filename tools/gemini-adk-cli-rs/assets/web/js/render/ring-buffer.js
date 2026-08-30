/**
 * Fixed-capacity ring buffer — O(1) push, O(1) indexed access.
 * Drops oldest items when full.
 *
 * @param {number} capacity - Maximum number of items
 */
function RingBuffer(capacity) {
  this._buf = new Array(capacity);
  this._cap = capacity;
  this._head = 0;   // next write position
  this._len = 0;
}

/**
 * Append an item. Drops oldest when full.
 * @param {*} item
 */
RingBuffer.prototype.push = function (item) {
  this._buf[this._head] = item;
  this._head = (this._head + 1) % this._cap;
  if (this._len < this._cap) this._len++;
};

/**
 * Logical index access: 0 = oldest, length-1 = newest.
 * @param {number} i
 * @returns {*}
 */
RingBuffer.prototype.get = function (i) {
  if (i < 0 || i >= this._len) return undefined;
  var start = (this._head - this._len + this._cap) % this._cap;
  return this._buf[(start + i) % this._cap];
};

/**
 * Return the most recent item, or undefined if empty.
 * @returns {*}
 */
RingBuffer.prototype.last = function () {
  if (this._len === 0) return undefined;
  return this._buf[(this._head - 1 + this._cap) % this._cap];
};

Object.defineProperty(RingBuffer.prototype, 'length', {
  get: function () { return this._len; }
});

/**
 * Reset the buffer to empty state.
 */
RingBuffer.prototype.clear = function () {
  this._head = 0;
  this._len = 0;
};

/**
 * Iterate oldest-first.
 * @param {function} fn - Called with (item, index)
 */
RingBuffer.prototype.forEach = function (fn) {
  var start = (this._head - this._len + this._cap) % this._cap;
  for (var i = 0; i < this._len; i++) {
    fn(this._buf[(start + i) % this._cap], i);
  }
};

/**
 * Return a new Array of items matching the predicate.
 * @param {function} pred - Called with (item, index), return truthy to include
 * @returns {Array}
 */
RingBuffer.prototype.filter = function (pred) {
  var result = [];
  var start = (this._head - this._len + this._cap) % this._cap;
  for (var i = 0; i < this._len; i++) {
    var item = this._buf[(start + i) % this._cap];
    if (pred(item, i)) result.push(item);
  }
  return result;
};

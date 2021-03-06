// Copyright (c) 2012 Ecma International.  All rights reserved.
// This code is governed by the BSD license found in the LICENSE file.

/*---
es5id: 15.4.4.20-3-9
description: >
    Array.prototype.filter - value of 'length' is a number (value is
    -Infinity)
---*/

        var accessed = false;
        function callbackfn(val, idx, obj) {
            accessed = true;
            return true;
        }

        var obj = { 0: 9, length: -Infinity };

        var newArr = Array.prototype.filter.call(obj, callbackfn);

assert.sameValue(newArr.length, 0, 'newArr.length');
assert.sameValue(accessed, false, 'accessed');

reportCompare(0, 0);

// Copyright (c) 2012 Ecma International.  All rights reserved.
// This code is governed by the BSD license found in the LICENSE file.

/*---
es5id: 15.4.4.16-7-c-i-9
description: >
    Array.prototype.every - element to be retrieved is own accessor
    property on an Array-like object
---*/

        function callbackfn(val, idx, obj) {
            if (idx === 0) {
                return val !== 11;
            } else {
                return true;
            }
        }

        var obj = { 10: 10, length: 20 };

        Object.defineProperty(obj, "0", {
            get: function () {
                return 11;
            },
            configurable: true
        });
        

assert.sameValue(Array.prototype.every.call(obj, callbackfn), false, 'Array.prototype.every.call(obj, callbackfn)');

reportCompare(0, 0);

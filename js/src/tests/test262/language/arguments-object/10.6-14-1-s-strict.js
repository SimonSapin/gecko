'use strict';
// Copyright (c) 2012 Ecma International.  All rights reserved.
// This code is governed by the BSD license found in the LICENSE file.

/*---
es5id: 10.6-14-1-s
description: Strict Mode - 'callee' exists under strict mode
flags: [onlyStrict]
---*/

        var argObj = function () {
            return arguments;
        } ();

assert(argObj.hasOwnProperty("callee"), 'argObj.hasOwnProperty("callee") !== true');

reportCompare(0, 0);
